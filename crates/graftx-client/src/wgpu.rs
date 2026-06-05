//! WebGPU client shim (Rust-level).
//!
//! Serializes WebGPU entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's WebGPU backend.
//! These are plain Rust functions; C-ABI export of the WebGPU entry points
//! comes later. This is a pure-Rust shim — no `wgpu`, Dawn, browser, or GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a WebGPU device handle.
const KIND_WGPU_DEVICE: u8 = 70;
/// Server object kind for a WebGPU buffer handle.
const KIND_WGPU_BUFFER: u8 = 71;

/// Issue a `requestDevice` call and return the new device [`Handle`](proto::Handle).
///
/// Sends an empty [`REQUEST_DEVICE`](proto::wgpu_op::REQUEST_DEVICE) request,
/// awaits the correlated response, validates its opcode and kind, decodes the
/// [`RequestDeviceResponse`](proto::wgpu::RequestDeviceResponse), and verifies
/// the returned handle names a WebGPU device.
pub fn request_device<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::wgpu_op::REQUEST_DEVICE,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::wgpu_op::REQUEST_DEVICE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::wgpu::RequestDeviceResponse::decode(b)?;
    if resp.device.kind() != KIND_WGPU_DEVICE {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.device.kind() as u16,
        )));
    }
    Ok(resp.device)
}

/// Issue a `createBuffer` call of `size` bytes with `usage` flags on `device`
/// and return the buffer [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_BUFFER`](proto::wgpu_op::CREATE_BUFFER) request carrying a
/// [`CreateBufferRequest`](proto::wgpu::CreateBufferRequest) naming `device`,
/// awaits the correlated response, validates its opcode and kind, decodes the
/// [`CreateBufferResponse`](proto::wgpu::CreateBufferResponse), and verifies the
/// returned handle names a WebGPU buffer.
pub fn create_buffer<T: Transport>(
    t: &mut T,
    device: proto::Handle,
    size: u64,
    usage: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::wgpu::CreateBufferRequest {
        device,
        size,
        usage,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::wgpu_op::CREATE_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::wgpu_op::CREATE_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::wgpu::CreateBufferResponse::decode(b)?;
    if resp.buffer.kind() != KIND_WGPU_BUFFER {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.buffer.kind() as u16,
        )));
    }
    Ok(resp.buffer)
}

/// Issue a `destroy` call for `buffer` and await its ack.
///
/// Sends a [`DESTROY_BUFFER`](proto::wgpu_op::DESTROY_BUFFER) request carrying a
/// [`DestroyBufferRequest`](proto::wgpu::DestroyBufferRequest) naming `buffer`,
/// awaits the correlated response, and validates its opcode and kind. The
/// response body is an empty ack.
pub fn destroy_buffer<T: Transport>(
    t: &mut T,
    buffer: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::wgpu::DestroyBufferRequest { buffer }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::wgpu_op::DESTROY_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::wgpu_op::DESTROY_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
