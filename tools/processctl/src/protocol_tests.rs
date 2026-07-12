use std::io::Cursor;
use std::path::PathBuf;

use crate::protocol::{read_frame, write_frame, Frame};
use crate::{ProcessIdentity, StartMarker};

#[test]
fn framed_identity_completion_and_failure_round_trip() {
    let frames = [
        Frame::Identity(ProcessIdentity {
            pid: 7,
            executable: PathBuf::from("/actual/executable"),
            started: StartMarker(11),
        }),
        Frame::Completion {
            raw_target_wait_status: 190 << 8,
            forced_remainder: false,
        },
        Frame::GuardianFailed("bounded failure".into()),
    ];
    let mut wire = Vec::new();
    for frame in &frames {
        write_frame(&mut wire, frame).unwrap();
    }
    let mut reader = Cursor::new(wire);
    assert!(matches!(
        read_frame(&mut reader).unwrap(),
        Frame::Identity(_)
    ));
    assert!(matches!(
        read_frame(&mut reader).unwrap(),
        Frame::Completion {
            raw_target_wait_status,
            forced_remainder: false
        } if raw_target_wait_status == 190 << 8
    ));
    assert!(matches!(
        read_frame(&mut reader).unwrap(),
        Frame::GuardianFailed(message) if message == "bounded failure"
    ));
}

#[test]
fn missing_truncated_and_oversized_frames_fail() {
    assert_eq!(
        read_frame(&mut Cursor::new(Vec::<u8>::new()))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );

    let mut truncated = Vec::new();
    write_frame(
        &mut truncated,
        &Frame::Completion {
            raw_target_wait_status: 1 << 8,
            forced_remainder: false,
        },
    )
    .unwrap();
    truncated.pop();
    assert_eq!(
        read_frame(&mut Cursor::new(truncated)).unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );

    let mut oversized = Vec::new();
    oversized.extend_from_slice(&u32::from_ne_bytes(*b"PCTL").to_ne_bytes());
    oversized.extend_from_slice(&[1, 2, 0, 0]);
    oversized.extend_from_slice(&(1024u32 * 1024 + 1).to_ne_bytes());
    assert_eq!(
        read_frame(&mut Cursor::new(oversized)).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}
