//! The POSIX guardian handshake/completion codec adapter, shared by the Linux
//! and macOS platform backends. Both re-exec the same binary as a guardian and
//! speak the same `crate::protocol` wire framing over the status pipe; only the
//! containment mechanism below the framing differs per OS. Keeping these two
//! readers in one place makes the wire contract a single authority that both
//! backends — and one shared test module — exercise.

use std::io::Read;
use std::process::ExitStatus;

use crate::protocol::{read_frame, Frame};
use crate::ProcessIdentity;

pub(super) fn read_handshake(reader: &mut impl Read) -> std::io::Result<ProcessIdentity> {
    match read_frame(reader)? {
        Frame::Identity(identity) => Ok(identity),
        Frame::GuardianFailed(message) => Err(std::io::Error::other(format!(
            "guardian failed before target handshake: {message}"
        ))),
        Frame::Completion { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "guardian sent completion before identity",
        )),
    }
}

pub(super) fn read_completion(reader: &mut impl Read) -> std::io::Result<(ExitStatus, bool)> {
    use std::os::unix::process::ExitStatusExt;
    match read_frame(reader)? {
        Frame::Completion {
            raw_target_wait_status,
            forced_remainder,
        } => Ok((
            ExitStatus::from_raw(raw_target_wait_status),
            forced_remainder,
        )),
        Frame::GuardianFailed(message) => Err(std::io::Error::other(format!(
            "process guardian failed: {message}"
        ))),
        Frame::Identity(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "guardian sent a second identity frame",
        )),
    }
}
