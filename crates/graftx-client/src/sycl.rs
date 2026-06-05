//! SYCL client shim (Rust-level).
//!
//! Serializes SYCL entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's SYCL backend.
//! These are plain Rust functions; C-ABI export of the SYCL entry points comes
//! later. This is a pure-Rust shim — no SYCL runtime, no driver, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a SYCL queue handle.
const KIND_SYCL_QUEUE: u8 = 90;
/// Server object kind for a SYCL device-pointer handle.
const KIND_SYCL_DEVICE_PTR: u8 = 91;

/// Create a SYCL queue and return its [`Handle`](proto::Handle).
///
/// Sends a [`QUEUE_CREATE`](proto::sycl_op::QUEUE_CREATE) request with an empty
/// body, awaits the correlated response, validates its opcode and kind, decodes
/// the [`QueueCreateResponse`](proto::sycl::QueueCreateResponse), and verifies
/// the returned handle names a SYCL queue.
pub fn queue_create<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::sycl_op::QUEUE_CREATE,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::sycl_op::QUEUE_CREATE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::sycl::QueueCreateResponse::decode(b)?;
    if resp.queue.kind() != KIND_SYCL_QUEUE {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.queue.kind() as u16,
        )));
    }
    Ok(resp.queue)
}

/// Allocate `size` bytes of device memory on `queue` and return the device
/// pointer [`Handle`](proto::Handle).
///
/// Sends a [`MALLOC_DEVICE`](proto::sycl_op::MALLOC_DEVICE) request carrying a
/// [`MallocDeviceRequest`](proto::sycl::MallocDeviceRequest) naming `queue`,
/// awaits the correlated response, validates its opcode and kind, decodes the
/// [`MallocDeviceResponse`](proto::sycl::MallocDeviceResponse), and verifies
/// the returned handle names a SYCL device pointer.
pub fn malloc_device<T: Transport>(
    t: &mut T,
    queue: proto::Handle,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::sycl::MallocDeviceRequest { queue, size }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::sycl_op::MALLOC_DEVICE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::sycl_op::MALLOC_DEVICE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::sycl::MallocDeviceResponse::decode(b)?;
    if resp.ptr.kind() != KIND_SYCL_DEVICE_PTR {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.ptr.kind() as u16,
        )));
    }
    Ok(resp.ptr)
}

/// Free the device pointer named by `ptr` and await its ack.
///
/// Sends a [`FREE`](proto::sycl_op::FREE) request carrying a
/// [`FreeRequest`](proto::sycl::FreeRequest) naming `ptr`, awaits the
/// correlated response, and validates its opcode and kind. The response body is
/// an empty ack.
pub fn free<T: Transport>(
    t: &mut T,
    ptr: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::sycl::FreeRequest { ptr }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::sycl_op::FREE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::sycl_op::FREE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
