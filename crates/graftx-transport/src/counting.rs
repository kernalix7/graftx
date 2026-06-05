//! Byte- and frame-counting [`Transport`] wrapper.
//!
//! Wraps any [`Transport`] and tallies the bytes and frames that flow through
//! it, so a backend can be instrumented without changing its behaviour. Counts
//! advance only on a successful [`send`](Transport::send) or
//! [`recv`](Transport::recv); a failed operation leaves every counter
//! untouched. All increments saturate, so a counter pins at [`u64::MAX`] rather
//! than wrapping.

use crate::Transport;
use std::io;

/// A [`Transport`] that counts the bytes and frames passing through `inner`.
///
/// Construct with [`new`](CountingTransport::new); recover the wrapped
/// transport with [`into_inner`](CountingTransport::into_inner). The four
/// getters report cumulative totals since construction.
pub struct CountingTransport<T> {
    inner: T,
    bytes_sent: u64,
    bytes_recv: u64,
    frames_sent: u64,
    frames_recv: u64,
}

impl<T> CountingTransport<T> {
    /// Wrap a transport, starting every counter at zero.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            bytes_sent: 0,
            bytes_recv: 0,
            frames_sent: 0,
            frames_recv: 0,
        }
    }

    /// Consume the wrapper and return the inner transport.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Total bytes successfully sent since construction.
    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    /// Total bytes successfully received since construction.
    pub fn bytes_recv(&self) -> u64 {
        self.bytes_recv
    }

    /// Total frames successfully sent since construction.
    pub fn frames_sent(&self) -> u64 {
        self.frames_sent
    }

    /// Total frames successfully received since construction.
    pub fn frames_recv(&self) -> u64 {
        self.frames_recv
    }
}

impl<T: Transport> Transport for CountingTransport<T> {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        self.inner.send(frame)?;
        self.bytes_sent = self.bytes_sent.saturating_add(frame.len() as u64);
        self.frames_sent = self.frames_sent.saturating_add(1);
        Ok(())
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let frame = self.inner.recv()?;
        self.bytes_recv = self.bytes_recv.saturating_add(frame.len() as u64);
        self.frames_recv = self.frames_recv.saturating_add(1);
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopback;

    #[test]
    fn counts_bytes_and_frames_through_loopback() {
        let (a, b) = loopback();
        let mut sender = CountingTransport::new(a);
        let mut receiver = CountingTransport::new(b);

        let frames: [&[u8]; 3] = [b"ping", b"second frame", &[0xab; 100]];
        let total_bytes: u64 = frames.iter().map(|f| f.len() as u64).sum();

        for frame in frames {
            sender.send(frame).expect("send frame");
        }
        for expected in frames {
            assert_eq!(receiver.recv().expect("recv frame"), expected);
        }

        assert_eq!(sender.bytes_sent(), total_bytes);
        assert_eq!(sender.frames_sent(), frames.len() as u64);
        // The sender never received anything.
        assert_eq!(sender.bytes_recv(), 0);
        assert_eq!(sender.frames_recv(), 0);

        assert_eq!(receiver.bytes_recv(), total_bytes);
        assert_eq!(receiver.frames_recv(), frames.len() as u64);
        // The receiver never sent anything.
        assert_eq!(receiver.bytes_sent(), 0);
        assert_eq!(receiver.frames_sent(), 0);
    }

    #[test]
    fn failed_send_leaves_counters_untouched() {
        let (a, b) = loopback();
        // Dropping the peer makes every subsequent send fail.
        drop(b);
        let mut sender = CountingTransport::new(a);

        assert!(sender.send(b"unreachable").is_err());
        assert_eq!(sender.bytes_sent(), 0);
        assert_eq!(sender.frames_sent(), 0);
    }

    #[test]
    fn into_inner_recovers_wrapped_transport() {
        let (a, mut b) = loopback();
        let mut sender = CountingTransport::new(a);
        sender.send(b"hi").expect("send frame");

        let mut inner = sender.into_inner();
        inner.send(b"direct").expect("send via inner");
        assert_eq!(b.recv().expect("recv first"), b"hi");
        assert_eq!(b.recv().expect("recv second"), b"direct");
    }
}
