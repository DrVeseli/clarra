use std::io::{ErrorKind, Read, Write};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::model::{Conflict, Revision};

pub const PROTOCOL_VERSION: u16 = 1;
pub const CHUNK_SIZE: usize = 256 * 1024;
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
pub enum Request {
    Hello {
        version: u16,
        device_id: String,
        device_token: String,
    },
    Announce {
        revisions: Vec<Revision>,
    },
    State,
    UploadStatus {
        hash: String,
        total_size: u64,
    },
    UploadChunk {
        hash: String,
        total_size: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    Download {
        hash: String,
        offset: u64,
    },
    Acknowledge {
        revision_ids: Vec<String>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub enum Response {
    Hello {
        project_id: String,
        sequence: u64,
    },
    State {
        sequence: u64,
        heads: Vec<Revision>,
        uploads: Vec<String>,
        downloads: Vec<Revision>,
        conflicts: Vec<Conflict>,
    },
    UploadOffset {
        offset: u64,
    },
    Chunk {
        offset: u64,
        total_size: u64,
        bytes: Vec<u8>,
    },
    Ok,
    Error {
        message: String,
    },
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let mut length = [0_u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_SIZE {
        bail!("protocol frame exceeds {MAX_FRAME_SIZE} bytes");
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    postcard::from_bytes(&payload)
        .context("invalid protocol frame")
        .map(Some)
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<()> {
    let payload = postcard::to_stdvec(message)?;
    if payload.len() > MAX_FRAME_SIZE {
        bail!("protocol frame exceeds {MAX_FRAME_SIZE} bytes");
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &Request::Hello {
                version: PROTOCOL_VERSION,
                device_id: "device".into(),
                device_token: "token".into(),
            },
        )
        .unwrap();
        let request: Request = read_frame(&mut bytes.as_slice()).unwrap().unwrap();
        assert!(matches!(request, Request::Hello { .. }));
    }
}
