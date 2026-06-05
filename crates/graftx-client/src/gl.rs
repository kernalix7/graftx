//! OpenGL client shim (Rust-level).
//!
//! Serializes OpenGL entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's OpenGL backend.
//! These are plain Rust functions; C-ABI export of the GL entry points comes
//! later. This is a pure-Rust shim — no GL loader, no driver, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for a GL context handle.
const KIND_GL_CONTEXT: u8 = 10;
/// Server object kind for a GL buffer handle.
const KIND_GL_BUFFER: u8 = 11;

/// Issue a context-creation call and return the new context [`Handle`](proto::Handle).
///
/// Sends an empty [`CREATE_CONTEXT`](proto::gl_op::CREATE_CONTEXT) request,
/// awaits the correlated response, validates its opcode and kind, decodes the
/// [`CreateContextResponse`](proto::gl::CreateContextResponse), and verifies the
/// returned handle names a GL context.
pub fn create_context<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::gl_op::CREATE_CONTEXT,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::gl_op::CREATE_CONTEXT || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::gl::CreateContextResponse::decode(b)?;
    if resp.context.kind() != KIND_GL_CONTEXT {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.context.kind() as u16,
        )));
    }
    Ok(resp.context)
}

/// Issue a make-current call for `context` and await its ack.
///
/// Sends a [`MAKE_CURRENT`](proto::gl_op::MAKE_CURRENT) request carrying a
/// [`MakeCurrentRequest`](proto::gl::MakeCurrentRequest) naming `context`,
/// awaits the correlated response, and validates its opcode and kind. The
/// response body is an empty ack.
pub fn make_current<T: Transport>(
    t: &mut T,
    context: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::gl::MakeCurrentRequest { context }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::gl_op::MAKE_CURRENT,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::gl_op::MAKE_CURRENT || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue a buffer-generation call in `context` and return the buffer [`Handle`](proto::Handle).
///
/// Sends a [`GEN_BUFFER`](proto::gl_op::GEN_BUFFER) request carrying a
/// [`GenBufferRequest`](proto::gl::GenBufferRequest) naming `context`, awaits the
/// correlated response, validates its opcode and kind, decodes the
/// [`GenBufferResponse`](proto::gl::GenBufferResponse), and verifies the returned
/// handle names a GL buffer.
pub fn gen_buffer<T: Transport>(
    t: &mut T,
    context: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::gl::GenBufferRequest { context }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::gl_op::GEN_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::gl_op::GEN_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::gl::GenBufferResponse::decode(b)?;
    if resp.buffer.kind() != KIND_GL_BUFFER {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.buffer.kind() as u16,
        )));
    }
    Ok(resp.buffer)
}
