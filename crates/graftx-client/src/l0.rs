//! Level Zero client shim (Rust-level).
//!
//! Serializes oneAPI Level Zero entrypoints into [`graftx_protocol`] frames and
//! forwards them over a [`graftx_transport::Transport`] to the server's Level
//! Zero backend. These are plain Rust functions; C-ABI export of the `ze*`
//! entry points comes later. This is a pure-Rust shim — no Level Zero SDK, no
//! GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a Level Zero context handle.
const KIND_CONTEXT: u8 = 50;
/// Server object kind for a Level Zero device-memory pointer handle.
const KIND_DEVICE_MEM: u8 = 51;

/// Issue `zeContextCreate` and return the new context [`Handle`](proto::Handle).
///
/// Sends a [`CONTEXT_CREATE`](proto::l0_op::CONTEXT_CREATE) request with an empty
/// body, awaits the correlated response, validates its opcode and kind, decodes
/// the [`ContextCreateResponse`](proto::l0::ContextCreateResponse), and verifies
/// the returned handle names a Level Zero context.
pub fn context_create<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::l0_op::CONTEXT_CREATE,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::l0_op::CONTEXT_CREATE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::l0::ContextCreateResponse::decode(b)?;
    if resp.context.kind() != KIND_CONTEXT {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.context.kind() as u16,
        )));
    }
    Ok(resp.context)
}

/// Issue `zeMemAllocDevice` and return the new device-pointer [`Handle`](proto::Handle).
///
/// Sends a [`MEM_ALLOC_DEVICE`](proto::l0_op::MEM_ALLOC_DEVICE) request carrying a
/// [`MemAllocDeviceRequest`](proto::l0::MemAllocDeviceRequest) naming `context`
/// and the allocation `size`, awaits the correlated response, validates its
/// opcode and kind, decodes the
/// [`MemAllocDeviceResponse`](proto::l0::MemAllocDeviceResponse), and verifies the
/// returned handle names a Level Zero device-memory pointer.
pub fn mem_alloc_device<T: Transport>(
    t: &mut T,
    context: proto::Handle,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::l0::MemAllocDeviceRequest { context, size }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::l0_op::MEM_ALLOC_DEVICE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::l0_op::MEM_ALLOC_DEVICE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::l0::MemAllocDeviceResponse::decode(b)?;
    if resp.ptr.kind() != KIND_DEVICE_MEM {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.ptr.kind() as u16,
        )));
    }
    Ok(resp.ptr)
}

/// Issue `zeMemFree`, releasing the device pointer named by `ptr`.
///
/// Sends a [`MEM_FREE`](proto::l0_op::MEM_FREE) request carrying a
/// [`MemFreeRequest`](proto::l0::MemFreeRequest), awaits the correlated response,
/// and validates its opcode and kind. The reply body is an empty acknowledgement,
/// so nothing is decoded.
pub fn mem_free<T: Transport>(
    t: &mut T,
    ptr: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::l0::MemFreeRequest { ptr }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::l0_op::MEM_FREE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::l0_op::MEM_FREE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
