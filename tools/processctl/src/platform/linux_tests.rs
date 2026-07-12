use std::io::Cursor;
use std::os::unix::process::ExitStatusExt;

use super::linux::{read_completion, read_handshake};
use crate::protocol::{write_frame, Frame};

#[test]
fn handshake_distinguishes_guardian_failure_from_identity() {
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::GuardianFailed("spawn denied".into())).unwrap();
    let error = read_handshake(&mut Cursor::new(wire)).unwrap_err();
    assert!(error.to_string().contains("spawn denied"));
}

#[test]
fn completion_preserves_exit_190_exit_1_and_signal_raw_status() {
    for raw in [190 << 8, 1 << 8, libc::SIGKILL] {
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &Frame::Completion {
                raw_target_wait_status: raw,
                forced_remainder: false,
            },
        )
        .unwrap();
        let (status, forced) = read_completion(&mut Cursor::new(wire)).unwrap();
        assert_eq!(status.into_raw(), raw);
        assert!(!forced);
    }
}

#[test]
fn completion_carries_forced_remainder_separately_from_target_status() {
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &Frame::Completion {
            raw_target_wait_status: 190 << 8,
            forced_remainder: true,
        },
    )
    .unwrap();
    let (status, forced) = read_completion(&mut Cursor::new(wire)).unwrap();
    assert_eq!(status.code(), Some(190));
    assert!(forced);
}

#[test]
fn guardian_failed_truncated_and_missing_completion_are_errors() {
    let mut failed = Vec::new();
    write_frame(
        &mut failed,
        &Frame::GuardianFailed("drain timed out".into()),
    )
    .unwrap();
    assert!(read_completion(&mut Cursor::new(failed))
        .unwrap_err()
        .to_string()
        .contains("drain timed out"));

    let mut truncated = Vec::new();
    write_frame(
        &mut truncated,
        &Frame::Completion {
            raw_target_wait_status: 0,
            forced_remainder: false,
        },
    )
    .unwrap();
    truncated.pop();
    assert_eq!(
        read_completion(&mut Cursor::new(truncated))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        read_completion(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}
