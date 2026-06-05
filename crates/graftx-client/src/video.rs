//! Video decode client shim (Rust-level).
//!
//! Serializes the video decode entrypoints into [`graftx_protocol`] frames and
//! forwards them over a [`graftx_transport::Transport`] to the server's video
//! backend. These are plain Rust functions; C-ABI export comes later. This is a
//! pure-Rust shim — no video runtime, no codec library, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a video decode-session handle.
const KIND_VIDEO_SESSION: u8 = 60;

/// Create a decode session for `codec` at `width`x`height` and return the new
/// session [`Handle`](proto::Handle).
///
/// Sends a
/// [`CREATE_DECODE_SESSION`](proto::video_op::CREATE_DECODE_SESSION) request
/// carrying a
/// [`CreateDecodeSessionRequest`](proto::video::CreateDecodeSessionRequest),
/// awaits the correlated response, validates its opcode and kind, decodes the
/// [`CreateDecodeSessionResponse`](proto::video::CreateDecodeSessionResponse),
/// and verifies the returned handle names a video session.
pub fn create_decode_session<T: Transport>(
    t: &mut T,
    codec: u32,
    width: u32,
    height: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::video::CreateDecodeSessionRequest {
        codec,
        width,
        height,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::video_op::CREATE_DECODE_SESSION,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::video_op::CREATE_DECODE_SESSION || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::video::CreateDecodeSessionResponse::decode(b)?;
    if resp.session.kind() != KIND_VIDEO_SESSION {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.session.kind() as u16,
        )));
    }
    Ok(resp.session)
}

/// Submit a `bitstream_len`-byte coded bitstream to `session`, decode one
/// frame, and return its output frame index.
///
/// Sends a [`DECODE_FRAME`](proto::video_op::DECODE_FRAME) request carrying a
/// [`DecodeFrameRequest`](proto::video::DecodeFrameRequest) naming `session`,
/// awaits the correlated response, validates its opcode and kind, and decodes
/// the [`DecodeFrameResponse`](proto::video::DecodeFrameResponse).
pub fn decode_frame<T: Transport>(
    t: &mut T,
    session: proto::Handle,
    bitstream_len: u32,
    req_id: u32,
    seq: u64,
) -> Result<u32, ClientError> {
    let mut body = Vec::new();
    proto::video::DecodeFrameRequest {
        session,
        bitstream_len,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::video_op::DECODE_FRAME,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::video_op::DECODE_FRAME || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::video::DecodeFrameResponse::decode(b)?;
    Ok(resp.frame_index)
}

/// Destroy the decode `session` and await its ack.
///
/// Sends a [`DESTROY_SESSION`](proto::video_op::DESTROY_SESSION) request
/// carrying a
/// [`DestroySessionRequest`](proto::video::DestroySessionRequest) naming
/// `session`, awaits the correlated response, and validates its opcode and
/// kind. The response body is an empty ack.
pub fn destroy_session<T: Transport>(
    t: &mut T,
    session: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::video::DestroySessionRequest { session }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::video_op::DESTROY_SESSION,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::video_op::DESTROY_SESSION || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
