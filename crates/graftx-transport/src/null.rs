//! Sink [`Transport`] with no peer.
//!
//! Discards everything sent and never delivers anything back. Use it to
//! benchmark the encode path in isolation, or in tests that only exercise the
//! send side, where the cost of a real backend would only add noise.

use crate::Transport;
use std::io;

/// A [`Transport`] that drops every sent frame and has no peer.
///
/// [`send`](Transport::send) always succeeds and discards the bytes;
/// [`recv`](Transport::recv) always fails with [`io::ErrorKind::UnexpectedEof`],
/// since no data will ever arrive.
pub struct NullTransport;

impl Transport for NullTransport {
    fn send(&mut self, _frame: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "null transport has no peer",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_succeeds_for_every_frame() {
        let mut transport = NullTransport;
        let frames: [&[u8]; 3] = [b"", b"first frame", &[0xcd; 256]];
        for frame in frames {
            transport.send(frame).expect("send frame");
        }
    }

    #[test]
    fn recv_reports_unexpected_eof() {
        let mut transport = NullTransport;
        let err = transport.recv().expect_err("recv must fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
