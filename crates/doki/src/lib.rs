pub mod agents;
pub mod background;
pub mod doctor;
pub mod sync;
pub mod tray;
pub mod undo;

use std::io::{self, Write};

pub fn write_quiet(mut out: impl Write, text: &str) -> io::Result<()> {
    match out.write_all(text.as_bytes()).and_then(|_| out.flush()) {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broken_pipe_is_not_an_error() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(write_quiet(Closed, "doki sync\n").is_ok());
        assert!(write_quiet(Vec::new(), "doki sync\n").is_ok());

        struct Denied;
        impl Write for Denied {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            write_quiet(Denied, "doki sync\n").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
