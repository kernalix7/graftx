//! AMF video-encode backend dispatch.
//!
//! The [`Session`](crate::Session) routes any AMF-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, CUDA, HIP, OpenCL, Level Zero, and video
//! backends, the AMF backend is a pure-Rust **stub**: it tracks encoder
//! lifetimes in a generational handle table and a per-encoder frame counter,
//! but performs no real codec or driver work. No AMF SDK, GPU, or FFI
//! dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for an AMF encoder handle.
const KIND_AMF_ENCODER: u8 = 100;

/// Server-side state tracked for one created AMF encoder.
#[derive(Debug)]
struct AmfEncoderState {
    /// Codec identifier the encoder produces. Recorded for the validation and
    /// driver bridge added in a later milestone; read only by tests for now.
    #[allow(dead_code)]
    codec: u32,
    /// Encoded frame width in pixels. Read only by tests for now.
    #[allow(dead_code)]
    width: u32,
    /// Encoded frame height in pixels. Read only by tests for now.
    #[allow(dead_code)]
    height: u32,
    /// Number of frames submitted to this encoder so far. Each `ENCODE_FRAME`
    /// advances it; read only by tests for now.
    #[allow(dead_code)]
    frames: u32,
}

/// Pure-Rust AMF encode backend stub.
///
/// Owns a generational handle table for the encoders it tracks. It answers
/// [`amf_op::CREATE_ENCODER`](proto::amf_op::CREATE_ENCODER) by minting an
/// encoder handle, [`amf_op::ENCODE_FRAME`](proto::amf_op::ENCODE_FRAME) by
/// echoing the input frame length back as the produced packet length and
/// advancing the encoder's frame counter, and
/// [`amf_op::DESTROY_ENCODER`](proto::amf_op::DESTROY_ENCODER) by removing a
/// known encoder handle and acknowledging with an empty body. The real codec
/// bridge lands in a later milestone; every other AMF call is reported as
/// not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct AmfBackend {
    encoders: HandleTable<AmfEncoderState>,
}

impl AmfBackend {
    /// Create a new AMF backend stub with an empty encoder table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            encoders: HandleTable::new(),
        }
    }
}

impl Backend for AmfBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::Amf
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the AMF namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Amf as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::amf_op::CREATE_ENCODER => {
                let req = proto::amf::CreateEncoderRequest::decode(body)?;
                let encoder = self.encoders.insert(
                    KIND_AMF_ENCODER,
                    AmfEncoderState {
                        codec: req.codec,
                        width: req.width,
                        height: req.height,
                        frames: 0,
                    },
                );
                let mut out = Vec::new();
                proto::amf::CreateEncoderResponse { encoder }.encode(&mut out);
                Ok(out)
            }
            proto::amf_op::ENCODE_FRAME => {
                let req = proto::amf::EncodeFrameRequest::decode(body)?;
                // The encoder must have been created on this backend and still
                // be live.
                let state = match self.encoders.get_mut(req.encoder) {
                    Some(state) => state,
                    None => return Err(proto::ProtocolError::UnknownOpcode(opcode)),
                };
                // Advance the per-encoder frame counter and echo the input
                // frame length back as the produced packet length.
                state.frames = state.frames.saturating_add(1);
                let mut out = Vec::new();
                proto::amf::EncodeFrameResponse {
                    packet_len: req.frame_len,
                }
                .encode(&mut out);
                Ok(out)
            }
            proto::amf_op::DESTROY_ENCODER => {
                let req = proto::amf::DestroyEncoderRequest::decode(body)?;
                // The encoder must have been created on this backend and still
                // be live; removing it both validates and destroys it.
                if self.encoders.remove(req.encoder).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the AMF namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `CREATE_ENCODER` request body.
    fn create_encoder_body(codec: u32, width: u32, height: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::amf::CreateEncoderRequest {
            codec,
            width,
            height,
        }
        .encode(&mut body);
        body
    }

    /// Encode an `ENCODE_FRAME` request body for `encoder` and `frame_len`.
    fn encode_frame_body(encoder: proto::Handle, frame_len: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::amf::EncodeFrameRequest { encoder, frame_len }.encode(&mut body);
        body
    }

    /// Encode a `DESTROY_ENCODER` request body for `encoder`.
    fn destroy_encoder_body(encoder: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::amf::DestroyEncoderRequest { encoder }.encode(&mut body);
        body
    }

    /// Drive a backend through `CREATE_ENCODER`, returning the minted encoder
    /// handle.
    fn create_encoder(backend: &mut AmfBackend) -> proto::Handle {
        let resp = backend
            .handle(
                proto::amf_op::CREATE_ENCODER,
                1,
                &create_encoder_body(1, 1920, 1080),
            )
            .expect("create encoder should succeed");
        proto::amf::CreateEncoderResponse::decode(&resp)
            .expect("decode create encoder response")
            .encoder
    }

    #[test]
    fn create_encoder_returns_kind_one_hundred() {
        let mut backend = AmfBackend::new();
        let resp = backend
            .handle(
                proto::amf_op::CREATE_ENCODER,
                1,
                &create_encoder_body(2, 1280, 720),
            )
            .expect("create encoder should succeed");

        let decoded = proto::amf::CreateEncoderResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.encoder.kind(), KIND_AMF_ENCODER);
        let state = backend
            .encoders
            .get(decoded.encoder)
            .expect("encoder state present");
        assert_eq!(state.codec, 2);
        assert_eq!(state.width, 1280);
        assert_eq!(state.height, 720);
        assert_eq!(state.frames, 0);
    }

    #[test]
    fn encode_frame_echoes_frame_len_and_bumps_counter() {
        let mut backend = AmfBackend::new();
        let encoder = create_encoder(&mut backend);

        let resp = backend
            .handle(
                proto::amf_op::ENCODE_FRAME,
                2,
                &encode_frame_body(encoder, 4096),
            )
            .expect("encode frame should succeed");
        let decoded = proto::amf::EncodeFrameResponse::decode(&resp).expect("decode response");
        // The produced packet length echoes the submitted frame length.
        assert_eq!(decoded.packet_len, 4096);

        // A second frame echoes its own length, and the counter has advanced.
        let resp = backend
            .handle(
                proto::amf_op::ENCODE_FRAME,
                3,
                &encode_frame_body(encoder, 8192),
            )
            .expect("encode frame should succeed");
        let decoded = proto::amf::EncodeFrameResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.packet_len, 8192);

        let state = backend
            .encoders
            .get(encoder)
            .expect("encoder state present");
        assert_eq!(state.frames, 2);
    }

    #[test]
    fn encode_frame_with_bogus_encoder_errors() {
        let mut backend = AmfBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_AMF_ENCODER, 0, 999);
        let err = backend
            .handle(
                proto::amf_op::ENCODE_FRAME,
                1,
                &encode_frame_body(bogus, 4096),
            )
            .expect_err("encode frame with bogus encoder must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_of_live_encoder_succeeds() {
        let mut backend = AmfBackend::new();
        let encoder = create_encoder(&mut backend);

        let resp = backend
            .handle(
                proto::amf_op::DESTROY_ENCODER,
                2,
                &destroy_encoder_body(encoder),
            )
            .expect("destroy encoder should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The encoder must no longer resolve.
        assert!(backend.encoders.get(encoder).is_none());
    }

    #[test]
    fn destroy_of_bogus_encoder_errors() {
        let mut backend = AmfBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_AMF_ENCODER, 0, 999);
        let err = backend
            .handle(
                proto::amf_op::DESTROY_ENCODER,
                1,
                &destroy_encoder_body(bogus),
            )
            .expect_err("destroy of bogus encoder must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_of_already_destroyed_encoder_errors() {
        let mut backend = AmfBackend::new();
        let encoder = create_encoder(&mut backend);

        backend
            .handle(
                proto::amf_op::DESTROY_ENCODER,
                2,
                &destroy_encoder_body(encoder),
            )
            .expect("first destroy should succeed");

        let err = backend
            .handle(
                proto::amf_op::DESTROY_ENCODER,
                3,
                &destroy_encoder_body(encoder),
            )
            .expect_err("second destroy must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = AmfBackend::new();
        // Opcode 0 is a core opcode, not in the AMF namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-amf opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
