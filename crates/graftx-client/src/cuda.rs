//! CUDA client shim (Rust-level).
//!
//! Serializes CUDA driver-API entrypoints into [`graftx_protocol`] frames and
//! forwards them over a [`graftx_transport::Transport`] to the server's CUDA
//! backend. These are plain Rust functions; C-ABI export of the CUDA entry
//! points comes later. This is a pure-Rust shim — no CUDA runtime, no driver,
//! no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a CUDA context handle.
const KIND_CUDA_CONTEXT: u8 = 20;
/// Server object kind for a CUDA device-pointer handle.
const KIND_CUDA_DEVICE_PTR: u8 = 21;

/// Issue a context-creation call for `device_ordinal` and return the new
/// context [`Handle`](proto::Handle).
///
/// Sends a [`CTX_CREATE`](proto::cuda_op::CTX_CREATE) request carrying a
/// [`CtxCreateRequest`](proto::cuda::CtxCreateRequest), awaits the correlated
/// response, validates its opcode and kind, decodes the
/// [`CtxCreateResponse`](proto::cuda::CtxCreateResponse), and verifies the
/// returned handle names a CUDA context.
pub fn ctx_create<T: Transport>(
    t: &mut T,
    device_ordinal: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::cuda::CtxCreateRequest { device_ordinal }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::cuda_op::CTX_CREATE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::cuda_op::CTX_CREATE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::cuda::CtxCreateResponse::decode(b)?;
    if resp.context.kind() != KIND_CUDA_CONTEXT {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.context.kind() as u16,
        )));
    }
    Ok(resp.context)
}

/// Issue a memory-allocation call of `size` bytes in `context` and return the
/// device-pointer [`Handle`](proto::Handle).
///
/// Sends a [`MEM_ALLOC`](proto::cuda_op::MEM_ALLOC) request carrying a
/// [`MemAllocRequest`](proto::cuda::MemAllocRequest) naming `context`, awaits
/// the correlated response, validates its opcode and kind, decodes the
/// [`MemAllocResponse`](proto::cuda::MemAllocResponse), and verifies the
/// returned handle names a CUDA device pointer.
pub fn mem_alloc<T: Transport>(
    t: &mut T,
    context: proto::Handle,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::cuda::MemAllocRequest { context, size }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::cuda_op::MEM_ALLOC,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::cuda_op::MEM_ALLOC || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::cuda::MemAllocResponse::decode(b)?;
    if resp.dptr.kind() != KIND_CUDA_DEVICE_PTR {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.dptr.kind() as u16,
        )));
    }
    Ok(resp.dptr)
}

/// Issue a memory-free call for `dptr` and await its ack.
///
/// Sends a [`MEM_FREE`](proto::cuda_op::MEM_FREE) request carrying a
/// [`MemFreeRequest`](proto::cuda::MemFreeRequest) naming `dptr`, awaits the
/// correlated response, and validates its opcode and kind. The response body
/// is an empty ack.
pub fn mem_free<T: Transport>(
    t: &mut T,
    dptr: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::cuda::MemFreeRequest { dptr }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::cuda_op::MEM_FREE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::cuda_op::MEM_FREE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
