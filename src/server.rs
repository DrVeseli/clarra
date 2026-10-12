use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::ledger::Ledger;
use serde::Serialize;

use crate::model::{Device, Event, Operation, ProjectState};
use crate::protocol::{CHUNK_SIZE, PROTOCOL_VERSION, Request, Response, read_frame, write_frame};

pub(crate) const DEFAULT_CACHE_LIMIT: u64 = 10 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ParticipantDiagnostics {
    pub id: String,
    pub name: String,
    pub devices: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Diagnostics {
    pub project_id: String,
    pub sequence: u64,
    pub participants: Vec<ParticipantDiagnostics>,
    pub devices: usize,
    pub files: usize,
    pub revisions: usize,
    pub conflicts: usize,
    pub pending_deliveries: usize,
    pub cache_bytes: u64,
    pub cache_limit: u64,
    pub storage_bytes: u64,
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
}

pub fn serve(project: &Path, mut input: impl Read, mut output: impl Write) -> Result<()> {
    let ledger = Ledger::open(project)?;
    let participant = effective_user_id();
    let mut device_id = None;

    while let Some(request) = read_frame::<Request>(&mut input)? {
        let response = handle_request(&ledger, &participant, &mut device_id, request)
            .unwrap_or_else(|error| Response::Error {
                message: format!("{error:#}"),
            });
        write_frame(&mut output, &response)?;
    }
    Ok(())
}

pub fn diagnostics(project: &Path) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(diagnostics_snapshot(project)?)?)
}

pub fn diagnostics_snapshot(project: &Path) -> Result<Diagnostics> {
    let ledger = Ledger::open(project)?;
    let state = ledger.read()?;
    let pending_deliveries: usize = state.pending.values().map(|devices| devices.len()).sum();
    let cache_bytes = directory_size(&ledger.root().join(".sync/cache"))?;
    let mut participants = BTreeMap::<_, usize>::new();
    for device in state.devices.values() {
        *participants.entry(device.participant.clone()).or_default() += 1;
    }
    let participants = participants
        .into_iter()
        .map(|(id, devices)| ParticipantDiagnostics {
            name: account_name(&id).unwrap_or_else(|| id.clone()),
            id,
            devices,
        })
        .collect();
    let files = state
        .heads
        .values()
        .filter(|heads| {
            heads.iter().any(|id| {
                state
                    .revisions
                    .get(id)
                    .is_some_and(|revision| revision.operation == Operation::Upsert)
            })
        })
        .count();
    let storage_bytes = cache_bytes
        .saturating_add(file_size(&ledger.root().join(".sync/ledger.log")))
        .saturating_add(file_size(&ledger.root().join(".sync/index/state.json")));
    let conflicts = state.conflicts().len();
    Ok(Diagnostics {
        project_id: state.project_id,
        sequence: state.sequence,
        participants,
        devices: state.devices.len(),
        files,
        revisions: state.revisions.len(),
        conflicts,
        pending_deliveries,
        cache_bytes,
        cache_limit: cache_limit(),
        storage_bytes,
        uploaded_bytes: state.uploaded_bytes,
        downloaded_bytes: state.downloaded_bytes,
    })
}

fn handle_request(
    ledger: &Ledger,
    participant: &str,
    session_device: &mut Option<String>,
    request: Request,
) -> Result<Response> {
    if let Request::Hello {
        version,
        device_id,
        device_token,
    } = request
    {
        if session_device.is_some() {
            bail!("session is already authenticated");
        }
        if version != PROTOCOL_VERSION {
            bail!("unsupported protocol version {version}");
        }
        if device_id.is_empty()
            || device_id.len() > 128
            || device_token.len() < 32
            || device_token.len() > 256
        {
            bail!("invalid device credentials");
        }
        let token_hash = blake3::hash(device_token.as_bytes()).to_hex().to_string();
        let state = ledger.read()?;
        let state = match state.devices.get(&device_id) {
            Some(device)
                if device.participant == participant && device.token_hash == token_hash =>
            {
                state
            }
            Some(_) => bail!("device identity is registered to different credentials"),
            None => ledger.append([Event::DeviceJoined(Device {
                id: device_id.clone(),
                participant: participant.into(),
                token_hash,
            })])?,
        };
        *session_device = Some(device_id);
        return Ok(Response::Hello {
            project_id: state.project_id,
            sequence: state.sequence,
        });
    }

    let device_id = session_device
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("HELLO must be the first message"))?;
    match request {
        Request::Hello { .. } => unreachable!(),
        Request::Announce { revisions } => {
            if revisions.len() > 1_000 {
                bail!("too many revisions in one announcement");
            }
            let current = ledger.read()?;
            let mut events = Vec::new();
            for revision in revisions {
                if revision.author_device != device_id {
                    bail!("revision author does not match this device");
                }
                revision.validate()?;
                if let Some(existing) = current.revisions.get(&revision.id) {
                    if existing != &revision {
                        bail!("revision ID reused with different metadata");
                    }
                } else {
                    events.push(Event::RevisionAnnounced(revision));
                }
            }
            if !events.is_empty() {
                ledger.append(events)?;
            }
            Ok(Response::Ok)
        }
        Request::State => {
            let state = ledger.read()?;
            let mut heads = state
                .heads
                .values()
                .flatten()
                .filter_map(|id| state.revisions.get(id))
                .cloned()
                .collect::<Vec<_>>();
            heads.sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
            Ok(Response::State {
                sequence: state.sequence,
                heads,
                uploads: state.uploads_for(device_id).into_iter().collect(),
                downloads: state.pending_for(device_id),
                conflicts: state.conflicts(),
            })
        }
        Request::UploadStatus { hash, total_size } => {
            validate_hash(&hash)?;
            let state = ledger.read()?;
            authorize_upload(&state, device_id, &hash, total_size)?;
            let final_path = ledger.cache_path(&hash);
            if final_path.is_file() {
                verify_file(&final_path, &hash, total_size)?;
                if !state.cached.contains(&hash) {
                    ledger.append([Event::ContentCached { hash }])?;
                }
                return Ok(Response::UploadOffset { offset: total_size });
            }
            let partial = partial_path(ledger, &hash, device_id);
            let offset = partial.metadata().map_or(0, |metadata| metadata.len());
            if offset > total_size {
                fs::remove_file(&partial)?;
                return Ok(Response::UploadOffset { offset: 0 });
            }
            if offset == total_size {
                if !partial.exists() {
                    File::create(&partial)?.sync_all()?;
                }
                verify_file(&partial, &hash, total_size)?;
                fs::rename(&partial, &final_path)?;
                ledger.append([Event::ContentCached { hash }])?;
            }
            Ok(Response::UploadOffset { offset })
        }
        Request::UploadChunk {
            hash,
            total_size,
            offset,
            bytes,
        } => {
            validate_hash(&hash)?;
            if bytes.len() > CHUNK_SIZE {
                bail!("upload chunk exceeds the protocol limit");
            }
            authorize_upload(&ledger.read()?, device_id, &hash, total_size)?;
            enforce_cache_limit(ledger, bytes.len() as u64)?;
            let partial = partial_path(ledger, &hash, device_id);
            let current_offset = partial.metadata().map_or(0, |metadata| metadata.len());
            if current_offset != offset {
                return Ok(Response::UploadOffset {
                    offset: current_offset,
                });
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&partial)?;
            file.write_all(&bytes)?;
            let new_offset = offset + bytes.len() as u64;
            if new_offset > total_size {
                bail!("upload exceeds its announced size");
            }
            if new_offset == total_size {
                file.sync_all()?;
                drop(file);
                verify_file(&partial, &hash, total_size)?;
                fs::rename(&partial, ledger.cache_path(&hash))?;
                ledger.append([
                    Event::DataTransferred {
                        uploaded_bytes: bytes.len() as u64,
                        downloaded_bytes: 0,
                    },
                    Event::ContentCached { hash },
                ])?;
            } else {
                ledger.append([Event::DataTransferred {
                    uploaded_bytes: bytes.len() as u64,
                    downloaded_bytes: 0,
                }])?;
            }
            Ok(Response::UploadOffset { offset: new_offset })
        }
        Request::Download { hash, offset } => {
            validate_hash(&hash)?;
            let state = ledger.read()?;
            if !device_may_download(&state, device_id, &hash) {
                bail!("content is not pending for this device");
            }
            let path = ledger.cache_path(&hash);
            let total_size = path.metadata()?.len();
            if offset > total_size {
                bail!("download offset exceeds object size");
            }
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; CHUNK_SIZE.min((total_size - offset) as usize)];
            file.read_exact(&mut bytes)?;
            if !bytes.is_empty() {
                ledger.append([Event::DataTransferred {
                    uploaded_bytes: 0,
                    downloaded_bytes: bytes.len() as u64,
                }])?;
            }
            Ok(Response::Chunk {
                offset,
                total_size,
                bytes,
            })
        }
        Request::Acknowledge { revision_ids } => {
            let before = ledger.read()?;
            let mut events = Vec::new();
            let mut hashes = Vec::new();
            for revision_id in revision_ids {
                let revision = before
                    .revisions
                    .get(&revision_id)
                    .with_context(|| format!("unknown revision {revision_id}"))?;
                if before
                    .pending
                    .get(&revision_id)
                    .is_some_and(|pending| pending.contains(device_id))
                {
                    events.push(Event::DeliveryAcknowledged {
                        device_id: device_id.into(),
                        revision_id,
                    });
                    if let Some(hash) = &revision.hash {
                        hashes.push(hash.clone());
                    }
                }
            }
            let mut state = if events.is_empty() {
                before
            } else {
                ledger.append(events)?
            };
            for hash in hashes {
                if !state.hash_still_needed(&hash) && ledger.cache_path(&hash).is_file() {
                    state = ledger.append([Event::CacheRemoved { hash: hash.clone() }])?;
                    fs::remove_file(ledger.cache_path(&hash))?;
                }
            }
            Ok(Response::Ok)
        }
    }
}

fn authorize_upload(
    state: &ProjectState,
    device_id: &str,
    hash: &str,
    total_size: u64,
) -> Result<()> {
    let authorized = state.revisions.values().any(|revision| {
        revision.hash.as_deref() == Some(hash)
            && revision.size == total_size
            && state
                .holders
                .get(&revision.id)
                .is_some_and(|holders| holders.contains(device_id))
            && state
                .pending
                .get(&revision.id)
                .is_some_and(|pending| !pending.is_empty())
    });
    if !authorized {
        bail!("device is not an eligible source for this content");
    }
    Ok(())
}

fn device_may_download(state: &ProjectState, device_id: &str, hash: &str) -> bool {
    state.revisions.values().any(|revision| {
        revision.operation == Operation::Upsert
            && revision.hash.as_deref() == Some(hash)
            && state
                .pending
                .get(&revision.id)
                .is_some_and(|pending| pending.contains(device_id))
    })
}

fn verify_file(path: &Path, expected_hash: &str, expected_size: u64) -> Result<()> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let copied = std::io::copy(&mut file, &mut hasher)?;
    if copied != expected_size || hasher.finalize().to_hex().as_str() != expected_hash {
        fs::remove_file(path)?;
        bail!("uploaded content failed size or hash verification");
    }
    Ok(())
}

fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid content hash");
    }
    Ok(())
}

fn partial_path(ledger: &Ledger, hash: &str, device_id: &str) -> std::path::PathBuf {
    let device = blake3::hash(device_id.as_bytes()).to_hex();
    ledger
        .root()
        .join(".sync/cache")
        .join(format!("{hash}.partial-{device}"))
}

fn enforce_cache_limit(ledger: &Ledger, incoming: u64) -> Result<()> {
    let limit = cache_limit();
    if directory_size(&ledger.root().join(".sync/cache"))?.saturating_add(incoming) > limit {
        bail!("server cache capacity exceeded");
    }
    Ok(())
}

fn cache_limit() -> u64 {
    std::env::var("CLARRA_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CACHE_LIMIT)
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut size: u64 = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            size = size.saturating_add(metadata.len());
        }
    }
    Ok(size)
}

fn file_size(path: &Path) -> u64 {
    path.metadata().map_or(0, |metadata| metadata.len())
}

fn account_name(id: &str) -> Option<String> {
    let uid = id.parse::<libc::uid_t>().ok()?;
    // SAFETY: getpwuid returns process-owned read-only storage; the string is copied immediately.
    let entry = unsafe { libc::getpwuid(uid) };
    if entry.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr((*entry).pw_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn effective_user_id() -> String {
    // SAFETY: geteuid has no preconditions and no mutable global state.
    unsafe { libc::geteuid() }.to_string()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::model::Revision;

    fn hello(
        ledger: &Ledger,
        participant: &str,
        session: &mut Option<String>,
        device_id: &str,
        token: &str,
    ) -> Result<Response> {
        handle_request(
            ledger,
            participant,
            session,
            Request::Hello {
                version: PROTOCOL_VERSION,
                device_id: device_id.into(),
                device_token: token.into(),
            },
        )
    }

    #[test]
    fn partial_upload_resumes_after_restart_and_credentials_are_bound() {
        let directory = tempdir().unwrap();
        let participant = effective_user_id();
        let alice_token = "a".repeat(32);
        let bob_token = "b".repeat(32);
        let content = b"abcdef";
        let hash = blake3::hash(content).to_hex().to_string();
        let ledger = Ledger::open(directory.path()).unwrap();
        let mut alice = None;
        hello(&ledger, &participant, &mut alice, "alice", &alice_token).unwrap();
        handle_request(
            &ledger,
            &participant,
            &mut alice,
            Request::Announce {
                revisions: vec![Revision {
                    id: "revision-1".into(),
                    path: "plan.md".into(),
                    hash: Some(hash.clone()),
                    size: content.len() as u64,
                    parents: vec![],
                    author_device: "alice".into(),
                    operation: Operation::Upsert,
                }],
            },
        )
        .unwrap();
        let mut bob = None;
        hello(&ledger, &participant, &mut bob, "bob", &bob_token).unwrap();
        let response = handle_request(
            &ledger,
            &participant,
            &mut alice,
            Request::UploadChunk {
                hash: hash.clone(),
                total_size: content.len() as u64,
                offset: 0,
                bytes: content[..3].to_vec(),
            },
        )
        .unwrap();
        assert!(matches!(response, Response::UploadOffset { offset: 3 }));
        drop(ledger);

        let ledger = Ledger::open(directory.path()).unwrap();
        let mut alice = None;
        hello(&ledger, &participant, &mut alice, "alice", &alice_token).unwrap();
        let response = handle_request(
            &ledger,
            &participant,
            &mut alice,
            Request::UploadStatus {
                hash: hash.clone(),
                total_size: content.len() as u64,
            },
        )
        .unwrap();
        assert!(matches!(response, Response::UploadOffset { offset: 3 }));
        handle_request(
            &ledger,
            &participant,
            &mut alice,
            Request::UploadChunk {
                hash: hash.clone(),
                total_size: content.len() as u64,
                offset: 3,
                bytes: content[3..].to_vec(),
            },
        )
        .unwrap();
        assert_eq!(fs::read(ledger.cache_path(&hash)).unwrap(), content);
        assert!(ledger.read().unwrap().cached.contains(&hash));

        let mut impersonator = None;
        assert!(
            hello(
                &ledger,
                &participant,
                &mut impersonator,
                "alice",
                &"wrong".repeat(8),
            )
            .is_err()
        );
    }
}
