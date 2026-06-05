//! HIP client shim (Rust-level).
//!
//! Serializes HIP runtime-API entrypoints into [`graftx_protocol`] frames and
//! forwards them over a [`graftx_transport::Transport`] to the server's HIP
//! backend. These are plain Rust functions; C-ABI export of the HIP entry
//! points comes later. This is a pure-Rust shim — no HIP runtime, no driver,
//! no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a HIP device-pointer handle.
const KIND_HIP_DEVICE_PTR: u8 = 30;
/// Server object kind for a HIP stream handle.
const KIND_HIP_STREAM: u8 = 31;

/// Issue a memory-allocation call of `size` bytes and return the device-pointer
/// [`Handle`](proto::Handle).
///
/// Sends a [`MALLOC`](proto::hip_op::MALLOC) request carrying a
/// [`MallocRequest`](proto::hip::MallocRequest), awaits the correlated
/// response, validates its opcode and kind, decodes the
/// [`MallocResponse`](proto::hip::MallocResponse), and verifies the returned
/// handle names a HIP device pointer.
pub fn malloc<T: Transport>(
    t: &mut T,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::hip::MallocRequest { size }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::hip_op::MALLOC,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::hip_op::MALLOC || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::hip::MallocResponse::decode(b)?;
    if resp.dptr.kind() != KIND_HIP_DEVICE_PTR {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.dptr.kind() as u16,
        )));
    }
    Ok(resp.dptr)
}

/// Issue a memory-free call for `dptr` and await its ack.
///
/// Sends a [`FREE`](proto::hip_op::FREE) request carrying a
/// [`FreeRequest`](proto::hip::FreeRequest) naming `dptr`, awaits the
/// correlated response, and validates its opcode and kind. The response body
/// is an empty ack.
pub fn free<T: Transport>(
    t: &mut T,
    dptr: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::hip::FreeRequest { dptr }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::hip_op::FREE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::hip_op::FREE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue a stream-creation call and return the new stream
/// [`Handle`](proto::Handle).
///
/// Sends a [`STREAM_CREATE`](proto::hip_op::STREAM_CREATE) request with an
/// empty body, awaits the correlated response, validates its opcode and kind,
/// decodes the [`StreamCreateResponse`](proto::hip::StreamCreateResponse), and
/// verifies the returned handle names a HIP stream.
pub fn stream_create<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::hip_op::STREAM_CREATE,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::hip_op::STREAM_CREATE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::hip::StreamCreateResponse::decode(b)?;
    if resp.stream.kind() != KIND_HIP_STREAM {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.stream.kind() as u16,
        )));
    }
    Ok(resp.stream)
}
