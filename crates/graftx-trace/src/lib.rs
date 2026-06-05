//! GraftX frame trace.
//!
//! Captures the protocol frames that pass over a [`Transport`] and replays the
//! recorded requests against another one. Wrap a live transport in a
//! [`RecordingTransport`] to tee every [`send`](Transport::send) and
//! [`recv`](Transport::recv) into a [`FrameTrace`]; later feed that trace to
//! [`replay`] to re-issue the captured request frames against a fresh
//! transport.
//!
//! A trace stores frames verbatim as their on-the-wire bytes, so a recording
//! is independent of the backend it came from: capture over a hypervisor link,
//! replay over an in-process [`loopback`](graftx_transport::loopback) in a
//! test. Direction is recovered from the protocol [`FrameHeader`]: replay
//! re-sends only the frames whose header kind is [`FrameKind::Request`], since
//! responses and events originate at the peer.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io;

use graftx_protocol::{decode_frame, FrameKind};
use graftx_transport::Transport;

/// An ordered log of protocol frames captured from a transport.
///
/// Frames are stored as their raw wire bytes in the order they were recorded.
/// The trace does not interpret them beyond what [`replay`] needs to tell a
/// request from a reply, so a malformed or truncated frame is still retained
/// faithfully and simply skipped at replay time.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FrameTrace {
    frames: Vec<Vec<u8>>,
}

impl FrameTrace {
    /// Create an empty trace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a copy of `frame` to the end of the trace.
    pub fn record(&mut self, frame: &[u8]) {
        self.frames.push(frame.to_vec());
    }

    /// Number of frames recorded so far.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the trace holds no frames.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Iterate over the recorded frames in capture order.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.frames.iter().map(Vec::as_slice)
    }
}

/// A [`Transport`] that tees every frame through `inner` into a [`FrameTrace`].
///
/// Construct with [`new`](RecordingTransport::new); a frame is recorded only
/// after the wrapped operation succeeds, so a failed
/// [`send`](Transport::send) or [`recv`](Transport::recv) leaves the trace
/// untouched. Both directions append to the same trace in the order calls
/// happen. Recover the captured trace with
/// [`into_trace`](RecordingTransport::into_trace) or borrow it live with
/// [`trace`](RecordingTransport::trace).
pub struct RecordingTransport<T> {
    inner: T,
    trace: FrameTrace,
}

impl<T> RecordingTransport<T> {
    /// Wrap a transport with a fresh, empty trace.
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            trace: FrameTrace::new(),
        }
    }

    /// Borrow the trace captured so far.
    pub fn trace(&self) -> &FrameTrace {
        &self.trace
    }

    /// Consume the wrapper and return the captured trace, dropping the inner
    /// transport.
    pub fn into_trace(self) -> FrameTrace {
        self.trace
    }

    /// Consume the wrapper and return the inner transport and its trace.
    pub fn into_parts(self) -> (T, FrameTrace) {
        (self.inner, self.trace)
    }
}

impl<T: Transport> Transport for RecordingTransport<T> {
    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        self.inner.send(frame)?;
        self.trace.record(frame);
        Ok(())
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let frame = self.inner.recv()?;
        self.trace.record(&frame);
        Ok(frame)
    }
}

/// Re-send every recorded request frame in `trace` over `t`, in capture order.
///
/// A frame counts as a request when its protocol [`FrameHeader`] decodes and
/// reports kind [`FrameKind::Request`]; responses, events, and any frame whose
/// header cannot be decoded are skipped, since only the client's requests
/// should be reissued. Replay sends but does not read: it never waits for a
/// reply, so a caller that needs the responses drains them from `t` itself.
/// The first failing [`send`](Transport::send) aborts replay and propagates its
/// error.
///
/// [`FrameHeader`]: graftx_protocol::FrameHeader
pub fn replay<T: Transport>(t: &mut T, trace: &FrameTrace) -> io::Result<()> {
    for frame in trace.iter() {
        if is_request(frame) {
            t.send(frame)?;
        }
    }
    Ok(())
}

/// Whether `frame` decodes to a request-kind protocol frame.
fn is_request(frame: &[u8]) -> bool {
    matches!(
        decode_frame(frame),
        Ok((header, _)) if header.kind == FrameKind::Request
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use graftx_protocol::{encode_frame, FrameHeader, PROTOCOL_MAJOR};
    use graftx_transport::{loopback, Transport};

    /// Build a wire frame with the given kind and request id over an empty body.
    fn frame(kind: FrameKind, req_id: u32) -> Vec<u8> {
        let header = FrameHeader {
            version: PROTOCOL_MAJOR,
            flags: 0,
            kind,
            opcode: 0x01_00_00_01,
            req_id,
            seq: u64::from(req_id),
            body_len: 0,
        };
        encode_frame(&header, &[])
    }

    #[test]
    fn new_trace_is_empty() {
        let trace = FrameTrace::new();
        assert!(trace.is_empty());
        assert_eq!(trace.len(), 0);
        assert_eq!(trace.iter().count(), 0);
    }

    #[test]
    fn record_appends_in_order() {
        let mut trace = FrameTrace::new();
        trace.record(b"first");
        trace.record(b"second");
        trace.record(b"third");

        assert_eq!(trace.len(), 3);
        assert!(!trace.is_empty());
        let collected: Vec<&[u8]> = trace.iter().collect();
        assert_eq!(collected, vec![&b"first"[..], b"second", b"third"]);
    }

    #[test]
    fn recording_transport_captures_both_directions() {
        let (a, mut b) = loopback();
        let mut rec = RecordingTransport::new(a);

        rec.send(b"req-1").expect("send req-1");
        rec.send(b"req-2").expect("send req-2");
        // Peer answers; the reply flows back through the recorder on recv.
        b.send(b"resp-1").expect("peer send");
        assert_eq!(rec.recv().expect("recv resp-1"), b"resp-1");
        assert_eq!(b.recv().expect("peer recv req-1"), b"req-1");
        assert_eq!(b.recv().expect("peer recv req-2"), b"req-2");

        let captured: Vec<&[u8]> = rec.trace().iter().collect();
        assert_eq!(captured, vec![&b"req-1"[..], b"req-2", b"resp-1"]);
    }

    #[test]
    fn failed_send_leaves_trace_untouched() {
        let (a, b) = loopback();
        // Dropping the peer makes every subsequent send fail.
        drop(b);
        let mut rec = RecordingTransport::new(a);

        assert!(rec.send(b"unreachable").is_err());
        assert!(rec.trace().is_empty());
    }

    #[test]
    fn into_parts_recovers_inner_and_trace() {
        let (a, mut b) = loopback();
        let mut rec = RecordingTransport::new(a);
        rec.send(b"hi").expect("send hi");

        let (mut inner, trace) = rec.into_parts();
        assert_eq!(trace.len(), 1);
        // The recovered inner transport still works.
        inner.send(b"direct").expect("send via inner");
        assert_eq!(b.recv().expect("recv first"), b"hi");
        assert_eq!(b.recv().expect("recv second"), b"direct");
    }

    #[test]
    fn replay_resends_only_request_frames() {
        // Capture a mixed trace: two requests interleaved with a response.
        let mut trace = FrameTrace::new();
        let req_a = frame(FrameKind::Request, 1);
        let resp = frame(FrameKind::Response, 1);
        let req_b = frame(FrameKind::Request, 2);
        trace.record(&req_a);
        trace.record(&resp);
        trace.record(&req_b);

        let (mut client, mut server) = loopback();
        replay(&mut client, &trace).expect("replay");
        drop(client);

        // Only the two request frames are re-sent, in order.
        assert_eq!(server.recv().expect("recv req_a"), req_a);
        assert_eq!(server.recv().expect("recv req_b"), req_b);
        assert!(server.recv().is_err(), "no further frames after requests");
    }

    #[test]
    fn replay_skips_undecodable_frames() {
        let mut trace = FrameTrace::new();
        // Too short to hold a header: must be skipped, not sent.
        trace.record(b"junk");
        let req = frame(FrameKind::Request, 7);
        trace.record(&req);

        let (mut client, mut server) = loopback();
        replay(&mut client, &trace).expect("replay");
        drop(client);

        assert_eq!(server.recv().expect("recv req"), req);
        assert!(server.recv().is_err(), "junk frame was not sent");
    }

    #[test]
    fn record_then_replay_round_trips_over_loopback() {
        // Record requests as they pass through a recorder, then replay the
        // captured trace against a fresh transport and confirm it arrives.
        let (a, mut peer) = loopback();
        let mut rec = RecordingTransport::new(a);
        let req = frame(FrameKind::Request, 42);
        rec.send(&req).expect("send req");
        assert_eq!(peer.recv().expect("peer recv"), req);

        let trace = rec.into_trace();
        let (mut client, mut server) = loopback();
        replay(&mut client, &trace).expect("replay");
        drop(client);
        assert_eq!(server.recv().expect("replayed recv"), req);
    }

    #[test]
    fn replay_empty_trace_sends_nothing() {
        let trace = FrameTrace::new();
        let (mut client, mut server) = loopback();
        replay(&mut client, &trace).expect("replay empty");
        drop(client);
        assert!(server.recv().is_err());
    }

    #[test]
    fn replay_propagates_send_error() {
        let mut trace = FrameTrace::new();
        trace.record(&frame(FrameKind::Request, 1));
        let (mut client, server) = loopback();
        // Drop the peer so the send fails.
        drop(server);
        assert!(replay(&mut client, &trace).is_err());
    }
}
