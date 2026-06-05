//! Per-session server state machine: decode → validate → replay.

use std::collections::HashMap;

use crate::Backend;
use graftx_protocol as proto;

/// One client↔server session. Owns the negotiated parameters, the outbound
/// sequence counter, and the per-API backend registry. Core opcodes
/// (`HELLO`/`NOOP`) are handled inline; every other opcode is routed to the
/// backend registered for its API namespace.
pub struct Session {
    /// Server-assigned id for this session.
    pub session_id: u64,
    /// Maximum control-frame body accepted, negotiated at handshake.
    pub max_frame_body: u32,
    /// Whether the handshake has completed.
    handshook: bool,
    /// Next outbound `seq` to stamp on a reply.
    next_seq: u64,
    /// Registered backends, keyed by API namespace byte ([`proto::opcode_api`]).
    backends: HashMap<u8, Box<dyn Backend>>,
}

impl Session {
    /// Create a session with the given server-assigned id.
    pub fn new(session_id: u64) -> Self {
        Self {
            session_id,
            max_frame_body: proto::DEFAULT_MAX_FRAME_BODY,
            handshook: false,
            next_seq: 1,
            backends: HashMap::new(),
        }
    }

    /// Register a backend for its API namespace. A later registration for the
    /// same namespace replaces the earlier one.
    pub fn register(&mut self, backend: Box<dyn Backend>) {
        let api = backend.api() as u8;
        self.backends.insert(api, backend);
    }

    fn next_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn reply(&mut self, opcode: u32, req_id: u32, body: &[u8]) -> Vec<u8> {
        let header = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Response,
            opcode,
            req_id,
            seq: self.next_seq(),
            body_len: body.len() as u32,
        };
        proto::encode_frame(&header, body)
    }

    /// Decode one client frame, validate it, replay it, and return the reply
    /// frame to send back. Every command is validated **before** it reaches any
    /// driver path (the client is untrusted).
    pub fn handle(&mut self, frame: &[u8]) -> Result<Vec<u8>, proto::ProtocolError> {
        let (h, body) = proto::decode_frame(frame)?;

        // Validate framing before acting on anything.
        if h.body_len > self.max_frame_body {
            return Err(proto::ProtocolError::FrameTooLarge(h.body_len));
        }
        if h.body_len as usize != body.len() {
            return Err(proto::ProtocolError::BodyLenMismatch {
                declared: h.body_len,
                actual: body.len(),
            });
        }

        match h.opcode {
            proto::core_op::HELLO => {
                let hello = proto::Hello::decode(body)?;
                if hello.proto_major != proto::PROTOCOL_MAJOR {
                    return Err(proto::ProtocolError::BadVersion {
                        major: hello.proto_major,
                        minor: hello.proto_minor,
                    });
                }
                self.max_frame_body = hello.max_frame_body.min(proto::DEFAULT_MAX_FRAME_BODY);
                self.handshook = true;
                let welcome = proto::Welcome {
                    proto_major: proto::PROTOCOL_MAJOR,
                    proto_minor: proto::PROTOCOL_MINOR,
                    features: 0,
                    max_frame_body: self.max_frame_body,
                    session_id: self.session_id,
                };
                let mut wb = Vec::new();
                welcome.encode(&mut wb);
                Ok(self.reply(proto::core_op::WELCOME, h.req_id, &wb))
            }
            proto::core_op::NOOP => Ok(self.reply(proto::core_op::NOOP, h.req_id, &[])),
            // Any non-core opcode is routed to the backend registered for its
            // API namespace; the backend returns the response body and the
            // session frames it (same opcode, same req_id, server-stamped seq).
            other => {
                let api = proto::opcode_api(other);
                match self.backends.get_mut(&api) {
                    Some(backend) => {
                        let resp = backend.handle(other, h.req_id, body)?;
                        Ok(self.reply(other, h.req_id, &resp))
                    }
                    None => Err(proto::ProtocolError::UnknownOpcode(other)),
                }
            }
        }
    }

    /// Whether the opening handshake has completed.
    pub fn is_handshook(&self) -> bool {
        self.handshook
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello_frame() -> Vec<u8> {
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
        proto::encode_frame(&header, &body)
    }

    #[test]
    fn hello_yields_welcome() {
        let mut s = Session::new(0x1234);
        let reply = s.handle(&hello_frame()).expect("handle hello");
        let (h, b) = proto::decode_frame(&reply).expect("decode reply");
        assert_eq!(h.opcode, proto::core_op::WELCOME);
        assert_eq!(h.kind, proto::FrameKind::Response);
        let w = proto::Welcome::decode(b).expect("welcome");
        assert_eq!(w.session_id, 0x1234);
        assert!(s.is_handshook());
    }

    fn request_frame(opcode: u32) -> Vec<u8> {
        let header = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Request,
            opcode,
            req_id: 7,
            seq: 1,
            body_len: 0,
        };
        proto::encode_frame(&header, &[])
    }

    #[test]
    fn registered_backend_enumerates_zero_devices() {
        let mut s = Session::new(1);
        s.register(Box::new(crate::VulkanBackend::new()));
        let frame = request_frame(proto::vk_op::ENUMERATE_PHYSICAL_DEVICES);
        let reply = s.handle(&frame).expect("handle enumerate");
        let (h, b) = proto::decode_frame(&reply).expect("decode reply");
        assert_eq!(h.opcode, proto::vk_op::ENUMERATE_PHYSICAL_DEVICES);
        assert_eq!(h.kind, proto::FrameKind::Response);
        assert_eq!(h.req_id, 7);
        assert_eq!(b, &0u32.to_le_bytes());
    }

    #[test]
    fn unregistered_api_opcode_is_unknown() {
        // No backend registered for the Vulkan namespace -> UnknownOpcode.
        let mut s = Session::new(1);
        let frame = request_frame(proto::vk_op::ENUMERATE_PHYSICAL_DEVICES);
        assert!(matches!(
            s.handle(&frame),
            Err(proto::ProtocolError::UnknownOpcode(_))
        ));
    }

    #[test]
    fn unknown_opcode_is_rejected() {
        let mut s = Session::new(1);
        let header = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Request,
            opcode: proto::opcode(proto::ApiId::Vulkan, 0xFFFF),
            req_id: 1,
            seq: 1,
            body_len: 0,
        };
        let frame = proto::encode_frame(&header, &[]);
        assert!(matches!(
            s.handle(&frame),
            Err(proto::ProtocolError::UnknownOpcode(_))
        ));
    }

    #[test]
    fn body_len_mismatch_is_rejected() {
        // Header claims 8 bytes of body but none follow.
        let header = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Request,
            opcode: proto::core_op::NOOP,
            req_id: 1,
            seq: 1,
            body_len: 8,
        };
        let frame = proto::encode_frame(&header, &[]);
        let mut s = Session::new(1);
        assert!(matches!(
            s.handle(&frame),
            Err(proto::ProtocolError::BodyLenMismatch { .. })
        ));
    }
}
