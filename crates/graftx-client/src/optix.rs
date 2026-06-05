//! OptiX client shim (Rust-level).
//!
//! Serializes OptiX entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's OptiX backend.
//! These are plain Rust functions; C-ABI export of the OptiX entry points comes
//! later. This is a pure-Rust shim — no OptiX SDK, no driver, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Server object kind for an OptiX device-context handle.
const KIND_OPTIX_CONTEXT: u8 = 80;
/// Server object kind for an OptiX ray-tracing pipeline handle.
const KIND_OPTIX_PIPELINE: u8 = 81;

/// Create an OptiX device context and return its [`Handle`](proto::Handle).
///
/// Sends a [`CONTEXT_CREATE`](proto::optix_op::CONTEXT_CREATE) request with an
/// empty body, awaits the correlated response, validates its opcode and kind,
/// decodes the [`ContextCreateResponse`](proto::optix::ContextCreateResponse),
/// and verifies the returned handle names an OptiX context.
pub fn context_create<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::optix_op::CONTEXT_CREATE,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::optix_op::CONTEXT_CREATE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::optix::ContextCreateResponse::decode(b)?;
    if resp.context.kind() != KIND_OPTIX_CONTEXT {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.context.kind() as u16,
        )));
    }
    Ok(resp.context)
}

/// Create a ray-tracing pipeline in `context` and return its
/// [`Handle`](proto::Handle).
///
/// Sends a [`PIPELINE_CREATE`](proto::optix_op::PIPELINE_CREATE) request
/// carrying a [`PipelineCreateRequest`](proto::optix::PipelineCreateRequest)
/// naming `context`, awaits the correlated response, validates its opcode and
/// kind, decodes the
/// [`PipelineCreateResponse`](proto::optix::PipelineCreateResponse), and
/// verifies the returned handle names an OptiX pipeline.
pub fn pipeline_create<T: Transport>(
    t: &mut T,
    context: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::optix::PipelineCreateRequest { context }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::optix_op::PIPELINE_CREATE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::optix_op::PIPELINE_CREATE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::optix::PipelineCreateResponse::decode(b)?;
    if resp.pipeline.kind() != KIND_OPTIX_PIPELINE {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.pipeline.kind() as u16,
        )));
    }
    Ok(resp.pipeline)
}

/// Destroy the OptiX context or pipeline named by `handle` and await its ack.
///
/// Sends a [`DESTROY`](proto::optix_op::DESTROY) request carrying a
/// [`DestroyRequest`](proto::optix::DestroyRequest) naming `handle`, awaits the
/// correlated response, and validates its opcode and kind. The response body is
/// an empty ack.
pub fn destroy<T: Transport>(
    t: &mut T,
    handle: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::optix::DestroyRequest { handle }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::optix_op::DESTROY,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::optix_op::DESTROY || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
