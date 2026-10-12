use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::{Conflict, INTERNAL_DIR, Operation, Revision, validate_relative_path};
use crate::protocol::{PROTOCOL_VERSION, Request, Response, read_frame, write_frame};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    Connected,
    FileReceived { path: String },
    FilePublished { path: String },
    Conflict(Conflict),
    Error { message: String },
}

#[derive(Clone)]
pub struct Engine {
    local: PathBuf,
    remote: String,
}

impl Engine {
    pub fn new(local: impl Into<PathBuf>, remote: impl Into<String>) -> Self {
        Self {
            local: local.into(),
            remote: remote.into(),
        }
    }

    pub fn sync_once(&self) -> Result<Vec<EngineEvent>> {
        let mut store = LocalStore::open(&self.local)?;
        let mut session = Session::connect(&self.remote)?;
        expect_hello(session.exchange(Request::Hello {
            version: PROTOCOL_VERSION,
            device_id: store.state.device_id.clone(),
            device_token: store.state.device_token.clone(),
        })?)?;
        let mut events = vec![EngineEvent::Connected];

        let initial = expect_state(session.exchange(Request::State)?)?;
        store.set_remote_heads(&initial.heads);
        let acknowledgments = store.scan(&initial.heads, &mut events)?;
        store.save()?;
        if !acknowledgments.is_empty() {
            expect_ok(session.exchange(Request::Acknowledge {
                revision_ids: acknowledgments,
            })?)?;
        }

        let outgoing = store.outgoing();
        for revisions in outgoing.chunks(1_000) {
            expect_ok(session.exchange(Request::Announce {
                revisions: revisions.to_vec(),
            })?)?;
        }
        store.clear_outbox();
        store.save()?;

        let state = expect_state(session.exchange(Request::State)?)?;
        for hash in &state.uploads {
            store.upload(&mut session, hash)?;
        }

        let mut received = Vec::new();
        for revision in &state.downloads {
            if let Some(hash) = &revision.hash {
                store.download(&mut session, hash)?;
            }
            store.apply(revision, &mut events)?;
            received.push(revision.id.clone());
        }
        store.set_remote_heads(&state.heads);
        for conflict in state.conflicts {
            events.push(EngineEvent::Conflict(conflict));
        }
        store.save()?;
        if !received.is_empty() {
            expect_ok(session.exchange(Request::Acknowledge {
                revision_ids: received,
            })?)?;
        }
        session.close()?;
        Ok(events)
    }

    pub fn watch(&self, mut on_event: impl FnMut(EngineEvent)) -> Result<()> {
        emit_result(self.sync_once(), &mut on_event);
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(&self.local, RecursiveMode::Recursive)?;

        loop {
            let should_sync = match receiver.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(event)) => event.paths.iter().any(|path| !is_engine_path(path)),
                Ok(Err(error)) => {
                    on_event(EngineEvent::Error {
                        message: error.to_string(),
                    });
                    false
                }
                Err(mpsc::RecvTimeoutError::Timeout) => true,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("filesystem watcher stopped")
                }
            };
            if should_sync {
                std::thread::sleep(Duration::from_millis(300));
                emit_result(self.sync_once(), &mut on_event);
            }
        }
    }

    pub fn resolve_conflict(&self, path: &str, content: &[u8]) -> Result<()> {
        validate_relative_path(path)?;
        let mut store = LocalStore::open(&self.local)?;
        let parents: Vec<String> = store
            .state
            .heads
            .get(path)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        if parents.len() < 2 {
            bail!("path does not have an unresolved conflict");
        }
        let hash = blake3::hash(content).to_hex().to_string();
        store.write_object(&hash, content)?;
        install_bytes(&store.root, path, content)?;
        let revision = Revision {
            id: Uuid::new_v4().to_string(),
            path: path.into(),
            hash: Some(hash.clone()),
            size: content.len() as u64,
            parents,
            author_device: store.state.device_id.clone(),
            operation: Operation::Upsert,
        };
        store.record_local_revision(revision);
        store.state.paths.insert(
            path.into(),
            PathState {
                installed_revision: store.state.heads[path].iter().next().cloned(),
                observed_hash: Some(hash),
            },
        );
        store.save()
    }
}

fn emit_result(result: Result<Vec<EngineEvent>>, callback: &mut impl FnMut(EngineEvent)) {
    match result {
        Ok(events) => events.into_iter().for_each(callback),
        Err(error) => callback(EngineEvent::Error {
            message: format!("{error:#}"),
        }),
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct LocalState {
    device_id: String,
    device_token: String,
    revisions: HashMap<String, Revision>,
    heads: HashMap<String, BTreeSet<String>>,
    paths: HashMap<String, PathState>,
    outbox: BTreeSet<String>,
    generated_conflicts: BTreeSet<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct PathState {
    installed_revision: Option<String>,
    observed_hash: Option<String>,
}

struct LocalStore {
    root: PathBuf,
    metadata: PathBuf,
    state: LocalState,
}

impl LocalStore {
    fn open(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot access local directory {}", root.display()))?;
        if !root.is_dir() {
            bail!("local path is not a directory");
        }
        let metadata = root.join(INTERNAL_DIR);
        reject_symlink(&metadata)?;
        fs::create_dir_all(metadata.join("objects"))?;
        fs::create_dir_all(metadata.join("tmp"))?;
        let state_path = metadata.join("state.json");
        let mut state: LocalState = match File::open(&state_path) {
            Ok(file) => serde_json::from_reader(BufReader::new(file))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => LocalState::default(),
            Err(error) => return Err(error.into()),
        };
        if state.device_id.is_empty() {
            state.device_id = Uuid::new_v4().to_string();
            state.device_token = format!("{}{}", Uuid::new_v4(), Uuid::new_v4());
        }
        let store = Self {
            root,
            metadata,
            state,
        };
        store.save()?;
        Ok(store)
    }

    fn save(&self) -> Result<()> {
        let temporary = self.metadata.join("state.json.tmp");
        let mut file = File::create(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        serde_json::to_writer(&mut file, &self.state)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, self.metadata.join("state.json"))?;
        Ok(())
    }

    fn set_remote_heads(&mut self, revisions: &[Revision]) {
        let mut heads: HashMap<String, BTreeSet<String>> = HashMap::new();
        for revision in revisions {
            self.state
                .revisions
                .insert(revision.id.clone(), revision.clone());
            heads
                .entry(revision.path.clone())
                .or_default()
                .insert(revision.id.clone());
        }
        for revision_id in &self.state.outbox {
            if let Some(revision) = self.state.revisions.get(revision_id) {
                let path_heads = heads.entry(revision.path.clone()).or_default();
                for parent in &revision.parents {
                    path_heads.remove(parent);
                }
                path_heads.insert(revision.id.clone());
            }
        }
        self.state.heads = heads;
    }

    fn scan(
        &mut self,
        remote_heads: &[Revision],
        events: &mut Vec<EngineEvent>,
    ) -> Result<Vec<String>> {
        let files = collect_files(&self.root, &self.state.generated_conflicts)?;
        let mut acknowledgments = Vec::new();
        for (path, source) in &files {
            let captured = self.capture(source)?;
            let existing = self.state.paths.get(path);
            if existing.and_then(|entry| entry.observed_hash.as_deref()) == Some(&captured.hash) {
                continue;
            }

            if existing.is_none()
                && let Some(revision) = remote_heads.iter().find(|revision| {
                    revision.path == *path && revision.hash.as_deref() == Some(&captured.hash)
                })
            {
                self.state
                    .revisions
                    .insert(revision.id.clone(), revision.clone());
                self.state.paths.insert(
                    path.clone(),
                    PathState {
                        installed_revision: Some(revision.id.clone()),
                        observed_hash: Some(captured.hash),
                    },
                );
                acknowledgments.push(revision.id.clone());
                continue;
            }

            let parents = existing
                .and_then(|entry| entry.installed_revision.clone())
                .into_iter()
                .collect();
            let revision = Revision {
                id: Uuid::new_v4().to_string(),
                path: path.clone(),
                hash: Some(captured.hash.clone()),
                size: captured.size,
                parents,
                author_device: self.state.device_id.clone(),
                operation: Operation::Upsert,
            };
            self.record_local_revision(revision.clone());
            self.state.paths.insert(
                path.clone(),
                PathState {
                    installed_revision: Some(revision.id),
                    observed_hash: Some(captured.hash),
                },
            );
            events.push(EngineEvent::FilePublished { path: path.clone() });
        }

        let removed: Vec<_> = self
            .state
            .paths
            .keys()
            .filter(|path| !files.contains_key(*path))
            .cloned()
            .collect();
        for path in removed {
            let Some(entry) = self.state.paths.get(&path) else {
                continue;
            };
            if entry.observed_hash.is_none() {
                continue;
            }
            let revision = Revision {
                id: Uuid::new_v4().to_string(),
                path: path.clone(),
                hash: None,
                size: 0,
                parents: entry.installed_revision.clone().into_iter().collect(),
                author_device: self.state.device_id.clone(),
                operation: Operation::Delete,
            };
            self.record_local_revision(revision.clone());
            self.state.paths.insert(
                path.clone(),
                PathState {
                    installed_revision: Some(revision.id),
                    observed_hash: None,
                },
            );
            events.push(EngineEvent::FilePublished { path });
        }
        Ok(acknowledgments)
    }

    fn capture(&self, source: &Path) -> Result<CapturedFile> {
        let before = source.metadata()?;
        let temporary = self.metadata.join("tmp").join(Uuid::new_v4().to_string());
        let mut input = File::open(source)?;
        let mut output = File::create(&temporary)?;
        let mut hasher = blake3::Hasher::new();
        let size = copy_and_hash(&mut input, &mut output, &mut hasher)?;
        output.sync_all()?;
        let after = source.metadata()?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            fs::remove_file(temporary)?;
            bail!("file changed while being captured: {}", source.display());
        }
        let hash = hasher.finalize().to_hex().to_string();
        let destination = self.object_path(&hash);
        if destination.exists() {
            fs::remove_file(temporary)?;
        } else {
            fs::rename(temporary, destination)?;
        }
        Ok(CapturedFile { hash, size })
    }

    fn record_local_revision(&mut self, revision: Revision) {
        let heads = self.state.heads.entry(revision.path.clone()).or_default();
        for parent in &revision.parents {
            heads.remove(parent);
        }
        heads.insert(revision.id.clone());
        self.state.outbox.insert(revision.id.clone());
        self.state.revisions.insert(revision.id.clone(), revision);
    }

    fn outgoing(&self) -> Vec<Revision> {
        let mut pending = self.state.outbox.clone();
        let mut outgoing = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let next = pending.iter().find(|id| {
                self.state.revisions[*id]
                    .parents
                    .iter()
                    .all(|parent| !pending.contains(parent))
            });
            let Some(next) = next.cloned() else {
                break;
            };
            pending.remove(&next);
            outgoing.push(self.state.revisions[&next].clone());
        }
        outgoing
    }

    fn clear_outbox(&mut self) {
        self.state.outbox.clear();
    }

    fn upload(&self, session: &mut Session, hash: &str) -> Result<()> {
        let path = self.object_path(hash);
        let total_size = path.metadata()?.len();
        let mut offset = expect_upload_offset(session.exchange(Request::UploadStatus {
            hash: hash.into(),
            total_size,
        })?)?;
        let mut file = File::open(path)?;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(offset))?;
        while offset < total_size {
            let mut bytes = vec![0; crate::protocol::CHUNK_SIZE];
            let read = file.read(&mut bytes)?;
            if read == 0 {
                bail!("retained object ended before its recorded size");
            }
            bytes.truncate(read);
            offset = expect_upload_offset(session.exchange(Request::UploadChunk {
                hash: hash.into(),
                total_size,
                offset,
                bytes,
            })?)?;
            file.seek(std::io::SeekFrom::Start(offset))?;
        }
        Ok(())
    }

    fn download(&self, session: &mut Session, hash: &str) -> Result<()> {
        let destination = self.object_path(hash);
        if destination.is_file() {
            return Ok(());
        }
        let partial = self.metadata.join("tmp").join(format!("{hash}.partial"));
        let mut offset = partial.metadata().map_or(0, |metadata| metadata.len());
        loop {
            let (response_offset, total_size, bytes) =
                expect_chunk(session.exchange(Request::Download {
                    hash: hash.into(),
                    offset,
                })?)?;
            if response_offset != offset {
                bail!("server returned a non-contiguous download chunk");
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&partial)?;
            file.write_all(&bytes)?;
            offset += bytes.len() as u64;
            if offset == total_size {
                file.sync_all()?;
                drop(file);
                verify_object(&partial, hash, total_size)?;
                fs::rename(partial, destination)?;
                return Ok(());
            }
            if bytes.is_empty() || offset > total_size {
                bail!("invalid download length");
            }
        }
    }

    fn apply(&mut self, revision: &Revision, events: &mut Vec<EngineEvent>) -> Result<()> {
        self.state
            .revisions
            .insert(revision.id.clone(), revision.clone());
        let installed = self
            .state
            .paths
            .get(&revision.path)
            .and_then(|path| path.installed_revision.as_deref());
        if installed == Some(&revision.id) {
            return Ok(());
        }
        let fast_forward = installed.is_none()
            || installed.is_some_and(|id| revision.parents.iter().any(|parent| parent == id));

        if fast_forward {
            match &revision.hash {
                Some(hash) => install_object(&self.root, &revision.path, &self.object_path(hash))?,
                None => remove_regular_file(&self.root, &revision.path)?,
            }
            self.state.paths.insert(
                revision.path.clone(),
                PathState {
                    installed_revision: Some(revision.id.clone()),
                    observed_hash: revision.hash.clone(),
                },
            );
            events.push(EngineEvent::FileReceived {
                path: revision.path.clone(),
            });
        } else if let Some(hash) = &revision.hash {
            let conflict_path = conflict_path(revision);
            install_object(&self.root, &conflict_path, &self.object_path(hash))?;
            self.state.generated_conflicts.insert(conflict_path.clone());
            events.push(EngineEvent::FileReceived {
                path: conflict_path,
            });
        }
        Ok(())
    }

    fn object_path(&self, hash: &str) -> PathBuf {
        self.metadata.join("objects").join(hash)
    }

    fn write_object(&self, hash: &str, content: &[u8]) -> Result<()> {
        let path = self.object_path(hash);
        if path.exists() {
            return Ok(());
        }
        let temporary = self.metadata.join("tmp").join(Uuid::new_v4().to_string());
        let mut file = File::create(&temporary)?;
        file.write_all(content)?;
        file.sync_all()?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

struct CapturedFile {
    hash: String,
    size: u64,
}

struct StateResponse {
    heads: Vec<Revision>,
    uploads: Vec<String>,
    downloads: Vec<Revision>,
    conflicts: Vec<Conflict>,
}

struct Session {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Session {
    fn connect(remote: &str) -> Result<Self> {
        let mut command = if let Some(project) = remote.strip_prefix("local:") {
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("attach").arg(project);
            command
        } else {
            let (host, project) = remote
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("remote must be [user@]host:/project/path"))?;
            if host.is_empty()
                || host.starts_with('-')
                || host.chars().any(|character| character.is_control())
                || project.is_empty()
            {
                bail!("invalid SSH remote");
            }
            let remote_command = format!("clarra attach -- {}", shell_quote(project));
            let mut command = Command::new("ssh");
            command.arg("--").arg(host).arg(remote_command);
            command
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("failed to start synchronization transport")?;
        let input = child.stdin.take().context("transport has no stdin")?;
        let output = child.stdout.take().context("transport has no stdout")?;
        Ok(Self {
            child,
            input: Some(input),
            output: BufReader::new(output),
        })
    }

    fn exchange(&mut self, request: Request) -> Result<Response> {
        write_frame(
            self.input.as_mut().context("transport is closed")?,
            &request,
        )?;
        match read_frame(&mut self.output)? {
            Some(Response::Error { message }) => bail!(message),
            Some(response) => Ok(response),
            None => bail!("transport closed without a response"),
        }
    }

    fn close(mut self) -> Result<()> {
        drop(self.input.take());
        let status = self.child.wait()?;
        if !status.success() {
            bail!("transport exited with {status}");
        }
        Ok(())
    }
}

fn expect_hello(response: Response) -> Result<()> {
    match response {
        Response::Hello { .. } => Ok(()),
        _ => bail!("server returned an unexpected HELLO response"),
    }
}

fn expect_state(response: Response) -> Result<StateResponse> {
    match response {
        Response::State {
            heads,
            uploads,
            downloads,
            conflicts,
            ..
        } => Ok(StateResponse {
            heads,
            uploads,
            downloads,
            conflicts,
        }),
        _ => bail!("server returned an unexpected state response"),
    }
}

fn expect_ok(response: Response) -> Result<()> {
    match response {
        Response::Ok => Ok(()),
        _ => bail!("server returned an unexpected acknowledgment"),
    }
}

fn expect_upload_offset(response: Response) -> Result<u64> {
    match response {
        Response::UploadOffset { offset } => Ok(offset),
        _ => bail!("server returned an unexpected upload response"),
    }
}

fn expect_chunk(response: Response) -> Result<(u64, u64, Vec<u8>)> {
    match response {
        Response::Chunk {
            offset,
            total_size,
            bytes,
        } => Ok((offset, total_size, bytes)),
        _ => bail!("server returned an unexpected download response"),
    }
}

fn collect_files(root: &Path, ignored: &BTreeSet<String>) -> Result<HashMap<String, PathBuf>> {
    fn visit(
        root: &Path,
        directory: &Path,
        ignored: &BTreeSet<String>,
        files: &mut HashMap<String, PathBuf>,
    ) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if path == root.join(INTERNAL_DIR) {
                    continue;
                }
                visit(root, &path, ignored, files)?;
            } else if metadata.is_file() {
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".clarra-"))
                {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF-8 filenames are not supported")?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                validate_relative_path(&relative)?;
                if !ignored.contains(&relative) {
                    files.insert(relative, path);
                }
            }
        }
        Ok(())
    }

    let mut files = HashMap::new();
    visit(root, root, ignored, &mut files)?;
    Ok(files)
}

fn copy_and_hash(input: &mut File, output: &mut File, hasher: &mut blake3::Hasher) -> Result<u64> {
    let mut size = 0;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            return Ok(size);
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
}

fn install_object(root: &Path, relative: &str, object: &Path) -> Result<()> {
    let mut input = File::open(object)?;
    install_reader(root, relative, &mut input)
}

fn install_bytes(root: &Path, relative: &str, content: &[u8]) -> Result<()> {
    let mut input = content;
    install_reader(root, relative, &mut input)
}

fn install_reader(root: &Path, relative: &str, input: &mut impl Read) -> Result<()> {
    let destination = safe_destination(root, relative)?;
    let parent = destination
        .parent()
        .context("file has no parent directory")?;
    create_safe_directories(root, parent)?;
    reject_symlink(&destination)?;
    let temporary = parent.join(format!(".clarra-{}.tmp", Uuid::new_v4()));
    let mut output = File::create(&temporary)?;
    std::io::copy(input, &mut output)?;
    output.sync_all()?;
    fs::rename(temporary, destination)?;
    Ok(())
}

fn remove_regular_file(root: &Path, relative: &str) -> Result<()> {
    let destination = safe_destination(root, relative)?;
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!("refusing to remove a symlink"),
        Ok(metadata) if metadata.is_file() => fs::remove_file(destination).map_err(Into::into),
        Ok(_) => bail!("refusing to replace a non-file path"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn safe_destination(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative_path(relative)?;
    Ok(root.join(relative))
}

fn create_safe_directories(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            reject_symlink(&current)?;
            if !current.exists() {
                fs::create_dir(&current)?;
            }
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => bail!("symlinks are not supported"),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn verify_object(path: &Path, expected_hash: &str, expected_size: u64) -> Result<()> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    if size != expected_size || hasher.finalize().to_hex().as_str() != expected_hash {
        fs::remove_file(path)?;
        bail!("downloaded object failed size or hash verification");
    }
    Ok(())
}

fn conflict_path(revision: &Revision) -> String {
    let path = Path::new(&revision.path);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let author: String = revision
        .author_device
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let revision_id: String = revision.id.chars().take(8).collect();
    let filename = format!("{stem}.clarra-conflict-{author}-{revision_id}{extension}");
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or(filename.clone(), |parent| {
            format!("{}/{}", parent.to_string_lossy(), filename)
        })
}

fn is_engine_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == INTERNAL_DIR)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names_are_deterministic_and_keep_extensions() {
        let revision = Revision {
            id: "12345678-more".into(),
            path: "docs/plan.md".into(),
            hash: Some("hash".into()),
            size: 1,
            parents: vec![],
            author_device: "alice-device".into(),
            operation: Operation::Upsert,
        };
        assert_eq!(
            conflict_path(&revision),
            "docs/plan.clarra-conflict-alicedev-12345678.md"
        );
    }
}
