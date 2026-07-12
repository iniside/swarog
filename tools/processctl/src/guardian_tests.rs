use std::io::Write;
use std::path::Path;

use crate::guardian::write_identity;

struct ClosedHandshake;

impl Write for ClosedHandshake {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "status reader closed",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn closed_status_pipe_aborts_identity_handshake() {
    let error =
        write_identity(&mut ClosedHandshake, 7, 11, Path::new("/actual/executable")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
}
