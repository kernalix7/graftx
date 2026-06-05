//! AMF video-encode client shim (Rust-level).
//!
//! Serializes the AMF encode entrypoints into [`graftx_protocol`] frames and
//! forwards them over a [`graftx_transport::Transport`] to the server's AMF
//! backend. These are plain Rust functions; C-ABI export comes later. This is a
//! pure-Rust shim — no AMF SDK, no codec library, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for an AMF encoder handle.
const KIND_AMF_ENCODER: u8 = 100;

/// Create an AMF encoder for `codec` at `width`x`height` and return the new
/// encoder [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_ENCODER`](proto::amf_op::CREATE_ENCODER) request carrying a
/// [`CreateEncoderRequest`](proto::amf::CreateEncoderRequest), awaits the
/// correlated response, validates its opcode and kind, decodes the
/// [`CreateEncoderResponse`](proto::amf::CreateEncoderResponse), and verifies
/// the returned handle names an AMF encoder.
pub fn create_encoder<T: Transport>(
    t: &mut T,
    codec: u32,
    width: u32,
    height: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::amf::CreateEncoderRequest {
        codec,
        width,
        height,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::amf_op::CREATE_ENCODER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::amf_op::CREATE_ENCODER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::amf::CreateEncoderResponse::decode(b)?;
    if resp.encoder.kind() != KIND_AMF_ENCODER {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.encoder.kind() as u16,
        )));
    }
    Ok(resp.encoder)
}

/// Submit a `frame_len`-byte raw frame to `encoder`, encode it, and return the
/// produced coded packet length.
///
/// Sends an [`ENCODE_FRAME`](proto::amf_op::ENCODE_FRAME) request carrying an
/// [`EncodeFrameRequest`](proto::amf::EncodeFrameRequest) naming `encoder`,
/// awaits the correlated response, validates its opcode and kind, and decodes
/// the [`EncodeFrameResponse`](proto::amf::EncodeFrameResponse).
pub fn encode_frame<T: Transport>(
    t: &mut T,
    encoder: proto::Handle,
    frame_len: u32,
    req_id: u32,
    seq: u64,
) -> Result<u32, ClientError> {
    let mut body = Vec::new();
    proto::amf::EncodeFrameRequest { encoder, frame_len }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::amf_op::ENCODE_FRAME,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::amf_op::ENCODE_FRAME || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::amf::EncodeFrameResponse::decode(b)?;
    Ok(resp.packet_len)
}

/// Destroy the `encoder` and await its ack.
///
/// Sends a [`DESTROY_ENCODER`](proto::amf_op::DESTROY_ENCODER) request carrying
/// a [`DestroyEncoderRequest`](proto::amf::DestroyEncoderRequest) naming
/// `encoder`, awaits the correlated response, and validates its opcode and
/// kind. The response body is an empty ack.
pub fn destroy_encoder<T: Transport>(
    t: &mut T,
    encoder: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::amf::DestroyEncoderRequest { encoder }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::amf_op::DESTROY_ENCODER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::amf_op::DESTROY_ENCODER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
