//! Length-prefixed [`Transport`] over any byte stream.
//!
//! Wraps any `Read + Write` (a TCP or vsock socket, a pipe, an in-memory
//! buffer) and frames messages with a 4-byte little-endian length header so a
//! single byte stream can carry discrete messages. This is the backend a real
//! virtio-vsock or TCP connection plugs into, complementing the in-process
//! [`Loopback`](crate::Loopback).

use std::io::{self, Read, Write};

use crate::Transport;

/// Upper bound on a single frame's length, in bytes (64 MiB).
///
/// Declared lengths above this are rejected on [`recv`](Transport::recv)
/// before any allocation, bounding the memory a peer can force us to reserve.
pub const MAX_FRAME: u32 = 64 * 1024 * 1024;

/// A message-framed [`Transport`] layered over a byte stream `S`.
///
/// Each [`send`](Transport::send) writes a 4-byte little-endian length header
/// followed by the frame bytes; each [`recv`](Transport::recv) reads the
/// header then exactly that many bytes. The framing is symmetric, so two
/// `StreamTransport`s over the two ends of a connected stream interoperate.
pub struct StreamTransport<S> {
    inner: S,
}

impl<S> StreamTransport<S> {
    /// Wrap a stream in a length-prefixed transport.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    /// Consume the transport and return the wrapped stream.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: Read + Write> Transport for StreamTransport<S> {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        let len = u32::try_from(frame.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "frame exceeds u32::MAX bytes")
        })?;
        self.inner.write_all(&len.to_le_bytes())?;
        self.inner.write_all(frame)?;
        self.inner.flush()
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 4];
        if let Err(err) = self.inner.read_exact(&mut header) {
            // A clean EOF on the header boundary means the peer closed between
            // frames; surface it as a definite end-of-stream rather than a
            // partial read.
            if err.kind() == io::ErrorKind::UnexpectedEof {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream closed before frame header",
                ));
            }
            return Err(err);
        }
        let len = u32::from_le_bytes(header);
        if len > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "declared frame length exceeds MAX_FRAME",
            ));
        }
        let mut frame = vec![0u8; len as usize];
        self.inner.read_exact(&mut frame)?;
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trips_frames_in_order() {
        let frames: [&[u8]; 4] = [b"", b"first", b"second frame", &[0xff; 1000]];

        let mut buf = Vec::new();
        {
            let mut writer = StreamTransport::new(Cursor::new(&mut buf));
            for frame in frames {
                writer.send(frame).expect("send frame");
            }
        }

        let mut reader = StreamTransport::new(Cursor::new(buf));
        for expected in frames {
            assert_eq!(reader.recv().expect("recv frame"), expected);
        }
    }

    #[test]
    fn rejects_oversized_declared_length() {
        let mut bytes = (MAX_FRAME + 1).to_le_bytes().to_vec();
        // No body follows; the length must be rejected before any read of it.
        bytes.extend_from_slice(b"unused");

        let mut reader = StreamTransport::new(Cursor::new(bytes));
        let err = reader
            .recv()
            .expect_err("oversized length must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_body_errors() {
        // Header declares 8 bytes but only 3 follow.
        let mut bytes = 8u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"abc");

        let mut reader = StreamTransport::new(Cursor::new(bytes));
        let err = reader.recv().expect_err("truncated body must error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn clean_eof_on_header_reports_unexpected_eof() {
        let mut reader = StreamTransport::new(Cursor::new(Vec::new()));
        let err = reader.recv().expect_err("empty stream must error");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn into_inner_returns_stream() {
        let transport = StreamTransport::new(Cursor::new(vec![1u8, 2, 3]));
        let cursor = transport.into_inner();
        assert_eq!(cursor.into_inner(), vec![1u8, 2, 3]);
    }
}
