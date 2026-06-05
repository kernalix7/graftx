//! In-memory queue [`Transport`] test double.
//!
//! A single-ended, deterministic stand-in for a real backend, with no peer
//! thread. Frames passed to [`send`](Transport::send) are appended to an
//! outbox that tests can inspect; frames enqueued ahead of time with
//! [`push_inbox`](VecTransport::push_inbox) are returned in order by
//! [`recv`](Transport::recv). Because nothing is ever transferred between the
//! two queues, behaviour is fully reproducible: a test scripts the inbox, runs
//! the code under test, then asserts on the outbox.

use crate::Transport;
use std::collections::VecDeque;
use std::io;

/// A [`Transport`] backed by two in-memory queues, for tests.
///
/// [`send`](Transport::send) appends each frame to the outbox; recover what
/// was sent with [`outbox`](VecTransport::outbox). [`recv`](Transport::recv)
/// pops the front of the inbox, returning [`io::ErrorKind::UnexpectedEof`] once
/// it is empty. The two queues are independent — a sent frame never reappears
/// from `recv` — so there is no peer and no concurrency.
#[derive(Debug, Default)]
pub struct VecTransport {
    outbox: Vec<Vec<u8>>,
    inbox: VecDeque<Vec<u8>>,
}

impl VecTransport {
    /// Create a transport with both queues empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `frame` to be returned by a later [`recv`](Transport::recv).
    ///
    /// Frames are returned in the order they are pushed.
    pub fn push_inbox(&mut self, frame: Vec<u8>) {
        self.inbox.push_back(frame);
    }

    /// Inspect, in send order, every frame passed to
    /// [`send`](Transport::send).
    pub fn outbox(&self) -> &[Vec<u8>] {
        &self.outbox
    }
}

impl Transport for VecTransport {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        self.outbox.push(frame.to_vec());
        Ok(())
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        self.inbox.pop_front().ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "vec transport inbox empty")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_inbox_then_recv_returns_in_order() {
        let mut transport = VecTransport::new();
        transport.push_inbox(b"first".to_vec());
        transport.push_inbox(b"second".to_vec());

        assert_eq!(transport.recv().expect("recv first"), b"first");
        assert_eq!(transport.recv().expect("recv second"), b"second");
    }

    #[test]
    fn recv_reports_unexpected_eof_when_empty() {
        let mut transport = VecTransport::new();
        let err = transport.recv().expect_err("recv must fail on empty inbox");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn recv_drains_inbox_then_errors() {
        let mut transport = VecTransport::new();
        transport.push_inbox(b"only".to_vec());

        assert_eq!(transport.recv().expect("recv queued frame"), b"only");
        let err = transport.recv().expect_err("recv must fail after draining");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn send_records_frames_into_outbox() {
        let mut transport = VecTransport::new();
        transport.send(b"ping").expect("send first");
        transport.send(b"second frame").expect("send second");

        assert_eq!(
            transport.outbox(),
            [b"ping".to_vec(), b"second frame".to_vec()]
        );
    }

    #[test]
    fn new_starts_with_empty_queues() {
        let transport = VecTransport::new();
        assert!(transport.outbox().is_empty());
    }

    #[test]
    fn send_and_recv_queues_are_independent() {
        let mut transport = VecTransport::new();
        transport.send(b"sent").expect("send frame");

        // A sent frame is not visible to recv; only pushed inbox frames are.
        let err = transport.recv().expect_err("outbox must not feed recv");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(transport.outbox(), [b"sent".to_vec()]);
    }
}
