//! GraftX guest-to-guest transport.
//!
//! Abstracts the low-latency channel that carries serialized GPU commands from
//! the Linux client to the Windows server and results back. Concrete backends
//! (virtio-vsock control plane, ivshmem shared-memory bulk plane) implement
//! [`Transport`]. Each [`Transport::send`] delivers exactly one framed message;
//! each [`Transport::recv`] returns exactly one.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io;
use std::sync::mpsc::{channel, Receiver, Sender};

mod stream;

pub use stream::{StreamTransport, MAX_FRAME};

/// A bidirectional, message-framed byte channel between two endpoints.
pub trait Transport {
    /// Send one framed message to the peer.
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Receive the next framed message from the peer, blocking until one
    /// arrives or the peer closes.
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}

/// An in-process, message-preserving transport used for host-only testing and
/// for exercising the client/server logic without a hypervisor.
///
/// Created in crossed pairs by [`loopback`]: bytes sent on one endpoint are
/// received on the other.
pub struct Loopback {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
}

/// Create a connected pair of [`Loopback`] endpoints.
pub fn loopback() -> (Loopback, Loopback) {
    let (a_tx, a_rx) = channel();
    let (b_tx, b_rx) = channel();
    (
        Loopback { tx: a_tx, rx: b_rx },
        Loopback { tx: b_tx, rx: a_rx },
    )
}

impl Transport for Loopback {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .send(frame.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "loopback peer closed"))
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        self.rx
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::UnexpectedEof, "loopback peer closed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_delivers_each_direction() {
        let (mut a, mut b) = loopback();
        a.send(b"ping").expect("send a->b");
        assert_eq!(b.recv().expect("recv b"), b"ping");
        b.send(b"pong").expect("send b->a");
        assert_eq!(a.recv().expect("recv a"), b"pong");
    }

    #[test]
    fn recv_errors_when_peer_dropped() {
        let (mut a, b) = loopback();
        drop(b);
        assert!(a.recv().is_err());
    }
}
