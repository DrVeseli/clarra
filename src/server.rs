use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::ledger::Ledger;
use crate::model::{Device, Event, Operation, ProjectState};
use crate::protocol::{CHUNK_SIZE, PROTOCOL_VERSION, Request, Response, read_frame, write_frame};

const DEFAULT_CACHE_LIMIT: u64 = 10 * 1024 * 1024 * 1024;

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
    let ledger = Ledger::open(project)?;
    let state = ledger.read()?;
    let pending_deliveries: usize = state.pending.values().map(|devices| devices.len()).sum();
    let cache_bytes = directory_size(&ledger.root().join(".sync/cache"))?;
    Ok(serde_json::json!({
        "project_id": state.project_id,
        "sequence": state.sequence,
        "devices": state.devices.len(),
        "revisions": state.revisions.len(),
        "conflicts": state.conflicts().len(),
        "pending_deliveries": pending_deliveries,
        "cache_bytes": cache_bytes,
    }))
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
            authorize_upload(&ledger.read()?, device_id, &hash, total_size)?;
            let final_path = ledger.cache_path(&hash);
            if final_path.is_file() {
                return Ok(Response::UploadOffset { offset: total_size });
            }
            let partial = partial_path(ledger, &hash);
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
            let partial = partial_path(ledger, &hash);
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
                ledger.append([Event::ContentCached { hash }])?;
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
                    fs::remove_file(ledger.cache_path(&hash))?;
                    state = ledger.append([Event::CacheRemoved { hash }])?;
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

fn partial_path(ledger: &Ledger, hash: &str) -> std::path::PathBuf {
    ledger
        .root()
        .join(".sync/cache")
        .join(format!("{hash}.partial"))
}

fn enforce_cache_limit(ledger: &Ledger, incoming: u64) -> Result<()> {
    let limit = std::env::var("GREEN_SYNC_CACHE_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_CACHE_LIMIT);
    if directory_size(&ledger.root().join(".sync/cache"))?.saturating_add(incoming) > limit {
        bail!("server cache capacity exceeded");
    }
    Ok(())
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

fn effective_user_id() -> String {
    // SAFETY: geteuid has no preconditions and no mutable global state.
    unsafe { libc::geteuid() }.to_string()
}
