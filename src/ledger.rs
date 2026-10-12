use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use uuid::Uuid;

use crate::model::{Event, EventEnvelope, ProjectState};

pub const SERVER_STATE_DIR: &str = ".sync";

pub struct Ledger {
    root: PathBuf,
    ledger_path: PathBuf,
    lock: File,
}

impl Ledger {
    pub fn open(project: impl AsRef<Path>) -> Result<Self> {
        let root = project
            .as_ref()
            .canonicalize()
            .with_context(|| format!("cannot access project {}", project.as_ref().display()))?;
        if !root.is_dir() {
            bail!("project path is not a directory");
        }

        let state_dir = root.join(SERVER_STATE_DIR);
        reject_symlink(&state_dir)?;
        reject_symlink(&state_dir.join("cache"))?;
        reject_symlink(&state_dir.join("index"))?;
        fs::create_dir_all(state_dir.join("cache"))?;
        fs::create_dir_all(state_dir.join("index"))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state_dir.join("lock"))?;
        let ledger = Self {
            root,
            ledger_path: state_dir.join("ledger.log"),
            lock,
        };
        ledger.ensure_initialized()?;
        Ok(ledger)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn cache_path(&self, hash: &str) -> PathBuf {
        self.root.join(SERVER_STATE_DIR).join("cache").join(hash)
    }

    pub fn read(&self) -> Result<ProjectState> {
        self.with_lock(|| self.load_unlocked())
    }

    pub fn append(&self, events: impl IntoIterator<Item = Event>) -> Result<ProjectState> {
        self.with_lock(|| {
            self.recover_tail_unlocked()?;
            let mut state = self.load_unlocked()?;
            let mut ledger = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.ledger_path)?;

            for event in events {
                let envelope = EventEnvelope {
                    id: Uuid::new_v4().to_string(),
                    sequence: state.sequence + 1,
                    event,
                };
                state.apply(&envelope)?;
                serde_json::to_writer(&mut ledger, &envelope)?;
                ledger.write_all(b"\n")?;
            }
            ledger.sync_all()?;
            self.write_index_unlocked(&state)?;
            Ok(state)
        })
    }

    fn ensure_initialized(&self) -> Result<()> {
        self.with_lock(|| {
            self.recover_tail_unlocked()?;
            if self.load_unlocked()?.project_id.is_empty() {
                let envelope = EventEnvelope {
                    id: Uuid::new_v4().to_string(),
                    sequence: 1,
                    event: Event::ProjectCreated {
                        project_id: Uuid::new_v4().to_string(),
                    },
                };
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.ledger_path)?;
                serde_json::to_writer(&mut file, &envelope)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                let mut state = ProjectState::default();
                state.apply(&envelope)?;
                self.write_index_unlocked(&state)?;
            }
            Ok(())
        })
    }

    fn with_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.lock.lock_exclusive()?;
        let result = operation();
        self.lock.unlock()?;
        result
    }

    fn load_unlocked(&self) -> Result<ProjectState> {
        let Ok(file) = File::open(&self.ledger_path) else {
            return Ok(ProjectState::default());
        };
        let mut state = ProjectState::default();
        for (line_number, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            let envelope: EventEnvelope = serde_json::from_str(&line)
                .with_context(|| format!("invalid ledger event on line {}", line_number + 1))?;
            state.apply(&envelope)?;
        }
        Ok(state)
    }

    fn recover_tail_unlocked(&self) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.ledger_path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.is_empty() || bytes.last() == Some(&b'\n') {
            return Ok(());
        }
        let valid_length = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        file.set_len(valid_length as u64)?;
        file.seek(SeekFrom::Start(valid_length as u64))?;
        file.sync_all()?;
        Ok(())
    }

    fn write_index_unlocked(&self, state: &ProjectState) -> Result<()> {
        let index = self.root.join(SERVER_STATE_DIR).join("index");
        let temporary = index.join("state.json.tmp");
        let destination = index.join("state.json");
        let mut file = File::create(&temporary)?;
        serde_json::to_writer(&mut file, state)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, destination)?;
        Ok(())
    }
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("server metadata directory must not be a symlink")
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;
    use crate::model::{Device, Event};

    #[test]
    fn ledger_recovers_an_incomplete_final_event() {
        let directory = tempdir().unwrap();
        let ledger = Ledger::open(directory.path()).unwrap();
        ledger
            .append([Event::DeviceJoined(Device {
                id: "alice".into(),
                participant: "501".into(),
                token_hash: "secret".into(),
            })])
            .unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(directory.path().join(".sync/ledger.log"))
            .unwrap();
        file.write_all(b"{partial").unwrap();

        let reopened = Ledger::open(directory.path()).unwrap();
        reopened
            .append([Event::DeviceJoined(Device {
                id: "bob".into(),
                participant: "502".into(),
                token_hash: "secret-2".into(),
            })])
            .unwrap();
        let state = reopened.read().unwrap();
        assert!(state.devices.contains_key("alice"));
        assert!(state.devices.contains_key("bob"));
    }
}
