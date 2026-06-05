//! Video decode backend dispatch.
//!
//! The [`Session`](crate::Session) routes any Video-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, CUDA, HIP, and OpenCL backends, the video
//! backend is a pure-Rust **stub**: it tracks decode-session lifetimes in a
//! generational handle table and a per-session frame counter, but performs no
//! real codec or driver work. No VA-API, VDPAU, NVDEC, or FFI dependency is
//! involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a video decode-session handle.
const KIND_VIDEO_SESSION: u8 = 60;

/// Server-side state tracked for one created video decode session.
#[derive(Debug)]
struct VideoSessionState {
    /// Codec identifier the session decodes. Recorded for the validation and
    /// driver bridge added in a later milestone; read only by tests for now.
    #[allow(dead_code)]
    codec: u32,
    /// Decoded frame width in pixels. Read only by tests for now.
    #[allow(dead_code)]
    width: u32,
    /// Decoded frame height in pixels. Read only by tests for now.
    #[allow(dead_code)]
    height: u32,
    /// Number of frames decoded so far; the next `DECODE_FRAME` returns this
    /// value as its `frame_index` and then increments it.
    frames: u32,
}

/// Pure-Rust video decode backend stub.
///
/// Owns a generational handle table for the decode sessions it tracks. It
/// answers
/// [`video_op::CREATE_DECODE_SESSION`](proto::video_op::CREATE_DECODE_SESSION)
/// by minting a session handle,
/// [`video_op::DECODE_FRAME`](proto::video_op::DECODE_FRAME) by returning the
/// session's current frame index and advancing the counter, and
/// [`video_op::DESTROY_SESSION`](proto::video_op::DESTROY_SESSION) by removing a
/// known session handle and acknowledging with an empty body. The real codec
/// bridge lands in a later milestone; every other video call is reported as
/// not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct VideoBackend {
    sessions: HandleTable<VideoSessionState>,
}

impl VideoBackend {
    /// Create a new video backend stub with an empty session table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: HandleTable::new(),
        }
    }
}

impl Backend for VideoBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::Video
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the Video namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Video as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::video_op::CREATE_DECODE_SESSION => {
                let req = proto::video::CreateDecodeSessionRequest::decode(body)?;
                let session = self.sessions.insert(
                    KIND_VIDEO_SESSION,
                    VideoSessionState {
                        codec: req.codec,
                        width: req.width,
                        height: req.height,
                        frames: 0,
                    },
                );
                let mut out = Vec::new();
                proto::video::CreateDecodeSessionResponse { session }.encode(&mut out);
                Ok(out)
            }
            proto::video_op::DECODE_FRAME => {
                let req = proto::video::DecodeFrameRequest::decode(body)?;
                // The session must have been created on this backend and still
                // be live.
                let state = match self.sessions.get_mut(req.session) {
                    Some(state) => state,
                    None => return Err(proto::ProtocolError::UnknownOpcode(opcode)),
                };
                // Take the current frame count as this frame's index, then
                // advance the counter for the next call.
                let frame_index = state.frames;
                state.frames = state.frames.saturating_add(1);
                let mut out = Vec::new();
                proto::video::DecodeFrameResponse { frame_index }.encode(&mut out);
                Ok(out)
            }
            proto::video_op::DESTROY_SESSION => {
                let req = proto::video::DestroySessionRequest::decode(body)?;
                // The session must have been created on this backend and still
                // be live; removing it both validates and destroys it.
                if self.sessions.remove(req.session).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the Video namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `CREATE_DECODE_SESSION` request body.
    fn create_session_body(codec: u32, width: u32, height: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::video::CreateDecodeSessionRequest {
            codec,
            width,
            height,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `DECODE_FRAME` request body.
    fn decode_frame_body(session: proto::Handle, bitstream_len: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::video::DecodeFrameRequest {
            session,
            bitstream_len,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `DESTROY_SESSION` request body.
    fn destroy_session_body(session: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::video::DestroySessionRequest { session }.encode(&mut body);
        body
    }

    /// Drive a backend through `CREATE_DECODE_SESSION`, returning the minted
    /// session handle.
    fn create_session(backend: &mut VideoBackend) -> proto::Handle {
        let resp = backend
            .handle(
                proto::video_op::CREATE_DECODE_SESSION,
                1,
                &create_session_body(7, 1920, 1080),
            )
            .expect("create decode session should succeed");
        proto::video::CreateDecodeSessionResponse::decode(&resp)
            .expect("decode create session response")
            .session
    }

    #[test]
    fn create_decode_session_returns_kind_sixty() {
        let mut backend = VideoBackend::new();
        let resp = backend
            .handle(
                proto::video_op::CREATE_DECODE_SESSION,
                1,
                &create_session_body(7, 640, 480),
            )
            .expect("create decode session should succeed");

        let decoded =
            proto::video::CreateDecodeSessionResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.session.kind(), KIND_VIDEO_SESSION);
        // The returned handle must resolve in the session table.
        assert!(backend.sessions.get(decoded.session).is_some());
    }

    #[test]
    fn decode_frame_returns_incrementing_indices() {
        let mut backend = VideoBackend::new();
        let session = create_session(&mut backend);

        let first = backend
            .handle(
                proto::video_op::DECODE_FRAME,
                2,
                &decode_frame_body(session, 256),
            )
            .expect("first decode frame should succeed");
        let first_index = proto::video::DecodeFrameResponse::decode(&first)
            .expect("decode first response")
            .frame_index;
        assert_eq!(first_index, 0);

        let second = backend
            .handle(
                proto::video_op::DECODE_FRAME,
                3,
                &decode_frame_body(session, 256),
            )
            .expect("second decode frame should succeed");
        let second_index = proto::video::DecodeFrameResponse::decode(&second)
            .expect("decode second response")
            .frame_index;
        assert_eq!(second_index, 1);
    }

    #[test]
    fn decode_frame_with_bogus_session_errors() {
        let mut backend = VideoBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_VIDEO_SESSION, 0, 999);
        let err = backend
            .handle(
                proto::video_op::DECODE_FRAME,
                1,
                &decode_frame_body(bogus, 128),
            )
            .expect_err("decode frame with bogus session must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_session_of_live_session_succeeds() {
        let mut backend = VideoBackend::new();
        let session = create_session(&mut backend);

        let resp = backend
            .handle(
                proto::video_op::DESTROY_SESSION,
                4,
                &destroy_session_body(session),
            )
            .expect("destroy session should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The session must no longer resolve.
        assert!(backend.sessions.get(session).is_none());
    }

    #[test]
    fn destroy_session_of_bogus_session_errors() {
        let mut backend = VideoBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_VIDEO_SESSION, 0, 999);
        let err = backend
            .handle(
                proto::video_op::DESTROY_SESSION,
                1,
                &destroy_session_body(bogus),
            )
            .expect_err("destroy session with bogus session must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = VideoBackend::new();
        // Opcode 0 is a core opcode, not in the Video namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-video opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
