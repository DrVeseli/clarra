use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const INTERNAL_DIR: &str = ".green-sync";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Operation {
    Upsert,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Revision {
    pub id: String,
    pub path: String,
    pub hash: Option<String>,
    pub size: u64,
    pub parents: Vec<String>,
    pub author_device: String,
    pub operation: Operation,
}

impl Revision {
    pub fn validate(&self) -> Result<()> {
        validate_relative_path(&self.path)?;
        match self.operation {
            Operation::Upsert if self.hash.is_none() => bail!("upsert revision has no hash"),
            Operation::Delete if self.hash.is_some() || self.size != 0 => {
                bail!("delete revision contains file content")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub participant: String,
    pub token_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Conflict {
    pub path: String,
    pub revision_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Event {
    ProjectCreated {
        project_id: String,
    },
    DeviceJoined(Device),
    RevisionAnnounced(Revision),
    DeliveryAcknowledged {
        device_id: String,
        revision_id: String,
    },
    ContentCached {
        hash: String,
    },
    CacheRemoved {
        hash: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EventEnvelope {
    pub id: String,
    pub sequence: u64,
    pub event: Event,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProjectState {
    pub project_id: String,
    pub sequence: u64,
    pub devices: HashMap<String, Device>,
    pub revisions: HashMap<String, Revision>,
    pub heads: HashMap<String, BTreeSet<String>>,
    pub pending: HashMap<String, BTreeSet<String>>,
    pub holders: HashMap<String, BTreeSet<String>>,
    pub cached: HashSet<String>,
    #[serde(skip)]
    applied_events: HashSet<String>,
}

impl ProjectState {
    pub fn apply(&mut self, envelope: &EventEnvelope) -> Result<()> {
        if self.applied_events.contains(&envelope.id) {
            return Ok(());
        }
        if envelope.sequence != self.sequence + 1 {
            bail!(
                "ledger sequence gap: expected {}, got {}",
                self.sequence + 1,
                envelope.sequence
            );
        }

        match &envelope.event {
            Event::ProjectCreated { project_id } => {
                if !self.project_id.is_empty() && self.project_id != *project_id {
                    bail!("project already initialized");
                }
                self.project_id.clone_from(project_id);
            }
            Event::DeviceJoined(device) => {
                if let Some(existing) = self.devices.get(&device.id) {
                    if existing != device {
                        bail!("device identity does not match its registration");
                    }
                } else {
                    self.devices.insert(device.id.clone(), device.clone());
                    for heads in self.heads.values() {
                        for revision_id in heads {
                            self.pending
                                .entry(revision_id.clone())
                                .or_default()
                                .insert(device.id.clone());
                        }
                    }
                }
            }
            Event::RevisionAnnounced(revision) => {
                revision.validate()?;
                if let Some(existing) = self.revisions.get(&revision.id) {
                    if existing != revision {
                        bail!("revision ID reused with different metadata");
                    }
                } else {
                    for parent in &revision.parents {
                        let parent_revision = self
                            .revisions
                            .get(parent)
                            .ok_or_else(|| anyhow::anyhow!("unknown parent revision {parent}"))?;
                        if parent_revision.path != revision.path {
                            bail!("parent belongs to a different path");
                        }
                    }
                    let heads = self.heads.entry(revision.path.clone()).or_default();
                    for parent in &revision.parents {
                        heads.remove(parent);
                    }
                    heads.insert(revision.id.clone());
                    for device_id in self.devices.keys() {
                        if device_id != &revision.author_device {
                            self.pending
                                .entry(revision.id.clone())
                                .or_default()
                                .insert(device_id.clone());
                        }
                    }
                    if revision.operation == Operation::Upsert {
                        self.holders
                            .entry(revision.id.clone())
                            .or_default()
                            .insert(revision.author_device.clone());
                    }
                    self.revisions.insert(revision.id.clone(), revision.clone());
                }
            }
            Event::DeliveryAcknowledged {
                device_id,
                revision_id,
            } => {
                if !self.revisions.contains_key(revision_id) {
                    bail!("acknowledgment references an unknown revision");
                }
                self.pending
                    .entry(revision_id.clone())
                    .or_default()
                    .remove(device_id);
                self.holders
                    .entry(revision_id.clone())
                    .or_default()
                    .insert(device_id.clone());
            }
            Event::ContentCached { hash } => {
                self.cached.insert(hash.clone());
            }
            Event::CacheRemoved { hash } => {
                self.cached.remove(hash);
            }
        }

        self.sequence = envelope.sequence;
        self.applied_events.insert(envelope.id.clone());
        Ok(())
    }

    pub fn conflicts(&self) -> Vec<Conflict> {
        let mut conflicts: Vec<_> = self
            .heads
            .iter()
            .filter(|(_, heads)| heads.len() > 1)
            .map(|(path, heads)| Conflict {
                path: path.clone(),
                revision_ids: heads.iter().cloned().collect(),
            })
            .collect();
        conflicts.sort_by(|a, b| a.path.cmp(&b.path));
        conflicts
    }

    pub fn pending_for(&self, device_id: &str) -> Vec<Revision> {
        let mut pending: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, devices)| devices.contains(device_id))
            .filter_map(|(revision_id, _)| self.revisions.get(revision_id))
            .filter(|revision| {
                revision.operation == Operation::Delete
                    || revision
                        .hash
                        .as_ref()
                        .is_some_and(|hash| self.cached.contains(hash))
            })
            .cloned()
            .collect();
        pending.sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
        let pending_ids: HashSet<_> = pending.iter().map(|revision| revision.id.clone()).collect();
        let mut delivered = HashSet::new();
        let mut revisions = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let position = pending.iter().position(|revision| {
                revision
                    .parents
                    .iter()
                    .all(|parent| !pending_ids.contains(parent) || delivered.contains(parent))
            });
            let Some(position) = position else {
                break;
            };
            let revision = pending.remove(position);
            delivered.insert(revision.id.clone());
            revisions.push(revision);
        }
        revisions
    }

    pub fn uploads_for(&self, device_id: &str) -> BTreeSet<String> {
        self.revisions
            .values()
            .filter(|revision| revision.operation == Operation::Upsert)
            .filter(|revision| {
                self.holders
                    .get(&revision.id)
                    .is_some_and(|holders| holders.contains(device_id))
            })
            .filter(|revision| {
                self.pending
                    .get(&revision.id)
                    .is_some_and(|pending| !pending.is_empty())
            })
            .filter_map(|revision| revision.hash.as_ref())
            .filter(|hash| !self.cached.contains(*hash))
            .cloned()
            .collect()
    }

    pub fn hash_still_needed(&self, hash: &str) -> bool {
        self.revisions.values().any(|revision| {
            revision.hash.as_deref() == Some(hash)
                && self
                    .pending
                    .get(&revision.id)
                    .is_some_and(|pending| !pending.is_empty())
        })
    }
}

pub fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty() || path.contains('\0') || path.contains('\\') {
        bail!("invalid portable path");
    }
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("path must stay inside the synchronized directory");
    }
    if candidate
        .components()
        .next()
        .is_some_and(|component| component.as_os_str() == INTERNAL_DIR)
    {
        bail!("path uses the engine metadata directory");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(state: &mut ProjectState, sequence: u64, event: Event) {
        state
            .apply(&EventEnvelope {
                id: format!("event-{sequence}"),
                sequence,
                event,
            })
            .unwrap();
    }

    fn revision(id: &str, author: &str, parents: &[&str]) -> Revision {
        Revision {
            id: id.into(),
            path: "plan.md".into(),
            hash: Some(format!("hash-{id}")),
            size: 1,
            parents: parents.iter().map(|parent| (*parent).into()).collect(),
            author_device: author.into(),
            operation: Operation::Upsert,
        }
    }

    #[test]
    fn concurrent_revisions_remain_heads_until_joined_by_a_resolution() {
        let mut state = ProjectState::default();
        apply(
            &mut state,
            1,
            Event::ProjectCreated {
                project_id: "project".into(),
            },
        );
        for (sequence, id) in [(2, "alice"), (3, "bob")] {
            apply(
                &mut state,
                sequence,
                Event::DeviceJoined(Device {
                    id: id.into(),
                    participant: id.into(),
                    token_hash: id.into(),
                }),
            );
        }
        apply(
            &mut state,
            4,
            Event::RevisionAnnounced(revision("v1", "alice", &[])),
        );
        apply(
            &mut state,
            5,
            Event::RevisionAnnounced(revision("v2-a", "alice", &["v1"])),
        );
        apply(
            &mut state,
            6,
            Event::RevisionAnnounced(revision("v2-b", "bob", &["v1"])),
        );
        assert_eq!(state.conflicts()[0].revision_ids, ["v2-a", "v2-b"]);

        apply(
            &mut state,
            7,
            Event::RevisionAnnounced(revision("v3", "alice", &["v2-a", "v2-b"])),
        );
        assert!(state.conflicts().is_empty());
    }

    #[test]
    fn traversal_and_internal_paths_are_rejected() {
        assert!(validate_relative_path("documents/plan.md").is_ok());
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path(".green-sync/state.json").is_err());
        assert!(validate_relative_path("C:\\secret").is_err());
    }
}
