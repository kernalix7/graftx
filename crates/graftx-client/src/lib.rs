//! GraftX client.
//!
//! Loaded into a Linux-guest application in place of the real GPU driver
//! libraries. Each shim intercepts an API's C entry points, serializes the
//! calls with [`graftx_protocol`], and forwards them over a
//! [`graftx_transport::Transport`] to the Windows-guest server.
//!
//! At M0 this crate provides the session bring-up: the [`handshake`] and a
//! [`noop`] round-trip that validate the protocol/transport pipe end to end.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io;

use graftx_protocol as proto;
use graftx_transport::Transport;

pub mod amf;
pub mod cl;
pub mod client;
pub mod cuda;
pub mod gl;
pub mod hip;
pub mod l0;
pub mod optix;
pub mod sycl;
pub mod video;
pub mod vk;
pub mod wgpu;

pub use client::Client;

/// Errors surfaced by the client session layer.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The transport failed.
    #[error("transport error: {0}")]
    Transport(#[from] io::Error),
    /// A frame could not be decoded.
    #[error("protocol error: {0}")]
    Protocol(#[from] proto::ProtocolError),
    /// The server replied with an unexpected opcode or kind.
    #[error("unexpected reply: opcode {opcode:#010x}, kind {kind:?}")]
    UnexpectedReply {
        /// Opcode the server sent.
        opcode: u32,
        /// Kind the server sent.
        kind: proto::FrameKind,
    },
}

/// Protocol version this client speaks.
pub fn protocol_version() -> (u16, u16) {
    (proto::PROTOCOL_MAJOR, proto::PROTOCOL_MINOR)
}

/// Perform the opening handshake: send `Hello`, await `Welcome`.
pub fn handshake<T: Transport>(t: &mut T) -> Result<proto::Welcome, ClientError> {
    let hello = proto::Hello {
        proto_major: proto::PROTOCOL_MAJOR,
        proto_minor: proto::PROTOCOL_MINOR,
        features: 0,
        max_frame_body: proto::DEFAULT_MAX_FRAME_BODY,
    };
    let mut body = Vec::new();
    hello.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::core_op::HELLO,
        req_id: 1,
        seq: 1,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::core_op::WELCOME || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(proto::Welcome::decode(b)?)
}

/// Issue a no-op request and await its response. Validates the pipe round-trip.
pub fn noop<T: Transport>(t: &mut T, req_id: u32, seq: u64) -> Result<(), ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::core_op::NOOP,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::core_op::NOOP || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
