use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use crate::{ProcessIdentity, StartMarker};

const MAGIC: u32 = u32::from_ne_bytes(*b"PCTL");
const VERSION: u8 = 1;
const HEADER_LEN: usize = 12;
const MAX_PAYLOAD: usize = 1024 * 1024;
const MAX_FAILURE: usize = 4096;

const IDENTITY: u8 = 1;
const COMPLETION: u8 = 2;
const GUARDIAN_FAILED: u8 = 3;

#[derive(Debug)]
pub(crate) enum Frame {
    Identity(ProcessIdentity),
    Completion {
        raw_target_wait_status: i32,
        forced_remainder: bool,
    },
    GuardianFailed(String),
}

pub(crate) fn write_frame(writer: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    let (kind, payload) = encode(frame)?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| invalid("frame too large"))?;
    writer.write_all(&MAGIC.to_ne_bytes())?;
    writer.write_all(&[VERSION, kind, 0, 0])?;
    writer.write_all(&payload_len.to_ne_bytes())?;
    writer.write_all(&payload)
}

pub(crate) fn read_frame(reader: &mut impl Read) -> std::io::Result<Frame> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if u32::from_ne_bytes(header[0..4].try_into().unwrap()) != MAGIC
        || header[4] != VERSION
        || header[6..8] != [0, 0]
    {
        return Err(invalid("invalid guardian frame header"));
    }
    let kind = header[5];
    let payload_len = u32::from_ne_bytes(header[8..12].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD {
        return Err(invalid("guardian frame exceeds the payload bound"));
    }
    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload)?;
    decode(kind, payload)
}

fn encode(frame: &Frame) -> std::io::Result<(u8, Vec<u8>)> {
    match frame {
        Frame::Identity(identity) => {
            let path = identity.executable.as_os_str().as_bytes();
            let path_len =
                u32::try_from(path.len()).map_err(|_| invalid("target path too long"))?;
            let mut payload = Vec::with_capacity(16 + path.len());
            payload.extend_from_slice(&identity.pid.to_ne_bytes());
            payload.extend_from_slice(&identity.started.0.to_ne_bytes());
            payload.extend_from_slice(&path_len.to_ne_bytes());
            payload.extend_from_slice(path);
            Ok((IDENTITY, payload))
        }
        Frame::Completion {
            raw_target_wait_status,
            forced_remainder,
        } => {
            let mut payload = Vec::with_capacity(5);
            payload.extend_from_slice(&raw_target_wait_status.to_ne_bytes());
            payload.push(u8::from(*forced_remainder));
            Ok((COMPLETION, payload))
        }
        Frame::GuardianFailed(message) => {
            let bytes = message.as_bytes();
            Ok((
                GUARDIAN_FAILED,
                bytes[..bytes.len().min(MAX_FAILURE)].to_vec(),
            ))
        }
    }
}

fn decode(kind: u8, payload: Vec<u8>) -> std::io::Result<Frame> {
    match kind {
        IDENTITY => {
            if payload.len() < 16 {
                return Err(invalid("identity frame is truncated"));
            }
            let pid = u32::from_ne_bytes(payload[0..4].try_into().unwrap());
            let started = u64::from_ne_bytes(payload[4..12].try_into().unwrap());
            let path_len = u32::from_ne_bytes(payload[12..16].try_into().unwrap()) as usize;
            if path_len == 0 || path_len != payload.len() - 16 {
                return Err(invalid("identity frame has an invalid path length"));
            }
            Ok(Frame::Identity(ProcessIdentity {
                pid,
                executable: PathBuf::from(OsString::from_vec(payload[16..].to_vec())),
                started: StartMarker(started),
            }))
        }
        COMPLETION => {
            if payload.len() != 5 || payload[4] > 1 {
                return Err(invalid("completion frame has an invalid payload"));
            }
            Ok(Frame::Completion {
                raw_target_wait_status: i32::from_ne_bytes(payload[0..4].try_into().unwrap()),
                forced_remainder: payload[4] == 1,
            })
        }
        GUARDIAN_FAILED => Ok(Frame::GuardianFailed(
            String::from_utf8_lossy(&payload).into_owned(),
        )),
        _ => Err(invalid("unknown guardian frame kind")),
    }
}

fn invalid(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}
