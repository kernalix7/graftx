//! OpenCL client shim (Rust-level).
//!
//! Serializes OpenCL entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's OpenCL backend.
//! These are plain Rust functions; C-ABI export of the OpenCL entry points
//! comes later. This is a pure-Rust shim — no OpenCL runtime, no ICD loader,
//! no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for an OpenCL context handle.
const KIND_CL_CONTEXT: u8 = 40;
/// Server object kind for an OpenCL memory-buffer handle.
const KIND_CL_MEM: u8 = 41;

/// Issue a context-creation call and return the new context
/// [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_CONTEXT`](proto::cl_op::CREATE_CONTEXT) request with an
/// empty body, awaits the correlated response, validates its opcode and kind,
/// decodes the [`CreateContextResponse`](proto::cl::CreateContextResponse),
/// and verifies the returned handle names an OpenCL context.
pub fn create_context<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::cl_op::CREATE_CONTEXT,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::cl_op::CREATE_CONTEXT || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::cl::CreateContextResponse::decode(b)?;
    if resp.context.kind() != KIND_CL_CONTEXT {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.context.kind() as u16,
        )));
    }
    Ok(resp.context)
}

/// Issue a buffer-creation call of `size` bytes in `context` and return the
/// memory-buffer [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_BUFFER`](proto::cl_op::CREATE_BUFFER) request carrying a
/// [`CreateBufferRequest`](proto::cl::CreateBufferRequest) naming `context`,
/// awaits the correlated response, validates its opcode and kind, decodes the
/// [`CreateBufferResponse`](proto::cl::CreateBufferResponse), and verifies the
/// returned handle names an OpenCL memory buffer.
pub fn create_buffer<T: Transport>(
    t: &mut T,
    context: proto::Handle,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::cl::CreateBufferRequest { context, size }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::cl_op::CREATE_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::cl_op::CREATE_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::cl::CreateBufferResponse::decode(b)?;
    if resp.mem.kind() != KIND_CL_MEM {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.mem.kind() as u16,
        )));
    }
    Ok(resp.mem)
}

/// Issue a buffer-release call for `mem` and await its ack.
///
/// Sends a [`RELEASE_BUFFER`](proto::cl_op::RELEASE_BUFFER) request carrying a
/// [`ReleaseBufferRequest`](proto::cl::ReleaseBufferRequest) naming `mem`,
/// awaits the correlated response, and validates its opcode and kind. The
/// response body is an empty ack.
pub fn release_buffer<T: Transport>(
    t: &mut T,
    mem: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::cl::ReleaseBufferRequest { mem }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::cl_op::RELEASE_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::cl_op::RELEASE_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
