//! GraftX guest-to-guest transport.
//!
//! Abstracts the low-latency channel that carries serialized GPU commands from
//! the Linux client to the Windows server and results back. Concrete backends
//! (virtio-vsock, ivshmem shared memory) implement [`Transport`].
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io;

/// A bidirectional, framed byte channel between the Linux client and the
/// Windows server.
pub trait Transport {
    /// Send one framed message to the peer.
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;

    /// Receive the next framed message from the peer.
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}
