//! Ergonomic session wrapper around a [`Transport`].
//!
//! The free functions in this crate (`handshake`, `noop`, and the per-API shims
//! in [`crate::vk`] and friends) each take an explicit `req_id`/`seq` pair so the
//! C-ABI export layer can drive correlation however it likes. [`Client`] is the
//! Rust-friendly front door: it owns the transport, runs the handshake on
//! [`Client::connect`], remembers the server-assigned session id, and allocates
//! `req_id`/`seq` values internally so callers never thread them by hand.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::{handshake, noop, vk, ClientError};

/// An open client session over a [`Transport`].
///
/// Construct one with [`Client::connect`], which performs the opening handshake
/// and stores the [`Welcome`](proto::Welcome)'s session id. Each request method
/// allocates the next `req_id`/`seq` from internal monotonic counters and
/// delegates to the corresponding free-function shim.
pub struct Client<T: Transport> {
    /// The underlying transport carrying encoded frames.
    transport: T,
    /// Next request id to hand out; monotonically increasing.
    next_req_id: u32,
    /// Next sequence number to hand out; monotonically increasing.
    next_seq: u64,
    /// Session id assigned by the server in its `Welcome`.
    session_id: u64,
}

impl<T: Transport> Client<T> {
    /// Open a session: run the handshake over `transport` and remember the
    /// server-assigned session id.
    ///
    /// The handshake itself uses `req_id`/`seq` of `1`, so the internal counters
    /// start at `2` to avoid colliding with it.
    pub fn connect(mut transport: T) -> Result<Client<T>, ClientError> {
        let welcome = handshake(&mut transport)?;
        Ok(Client {
            transport,
            next_req_id: 2,
            next_seq: 2,
            session_id: welcome.session_id,
        })
    }

    /// The server-assigned session id from the handshake.
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    /// Allocate the next `(req_id, seq)` correlation pair, advancing both
    /// counters. Counters saturate rather than wrap so correlation ids stay
    /// monotonic for the life of the session.
    fn next_ids(&mut self) -> (u32, u64) {
        let req_id = self.next_req_id;
        let seq = self.next_seq;
        self.next_req_id = self.next_req_id.saturating_add(1);
        self.next_seq = self.next_seq.saturating_add(1);
        (req_id, seq)
    }

    /// Issue a no-op round-trip, validating the pipe end to end.
    pub fn noop(&mut self) -> Result<(), ClientError> {
        let (req_id, seq) = self.next_ids();
        noop(&mut self.transport, req_id, seq)
    }

    /// Issue `vkCreateInstance` and return the new instance [`Handle`](proto::Handle).
    pub fn vk_create_instance(
        &mut self,
        app_api_version: u32,
    ) -> Result<proto::Handle, ClientError> {
        let (req_id, seq) = self.next_ids();
        vk::create_instance(&mut self.transport, app_api_version, req_id, seq)
    }

    /// Issue `vkEnumeratePhysicalDevices` for `instance` and return the
    /// physical-device [`Handle`](proto::Handle)s.
    pub fn vk_enumerate_physical_devices(
        &mut self,
        instance: proto::Handle,
    ) -> Result<Vec<proto::Handle>, ClientError> {
        let (req_id, seq) = self.next_ids();
        vk::enumerate_physical_devices(&mut self.transport, instance, req_id, seq)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use graftx_protocol as proto;
    use graftx_transport::{loopback, Transport};

    use super::Client;

    /// Server object kind for a `VkInstance` handle (mirrors the server backend).
    const KIND_INSTANCE: u8 = 1;
    /// Server object kind for a `VkPhysicalDevice` handle.
    const KIND_PHYSICAL_DEVICE: u8 = 2;

    /// Build a `Response` frame echoing the request's opcode, `req_id`, and `seq`.
    fn response_frame(req: &proto::FrameHeader, body: &[u8]) -> Vec<u8> {
        let header = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Response,
            opcode: req.opcode,
            req_id: req.req_id,
            seq: req.seq,
            body_len: body.len() as u32,
        };
        proto::encode_frame(&header, body)
    }

    /// Drive `connect` -> `noop` -> `vk_create_instance` ->
    /// `vk_enumerate_physical_devices` against an inline loopback responder.
    ///
    /// The responder asserts the expected opcode sequence and that the wrapper's
    /// internal counters hand out distinct, monotonic `req_id`/`seq` values, then
    /// returns canned `Welcome`/handle frames.
    #[test]
    fn client_drives_handshake_noop_and_vk_calls() {
        let (client_t, mut server) = loopback();
        let session_id = 0xABCD_1234_5678_9ABC;
        let instance = proto::Handle::new(KIND_INSTANCE, 1, 7);
        let device = proto::Handle::new(KIND_PHYSICAL_DEVICE, 1, 11);

        let responder = thread::spawn(move || {
            // 1. Handshake: Hello -> Welcome. Uses the fixed req_id/seq of 1.
            let frame = server.recv().expect("responder recv hello");
            let (h, _) = proto::decode_frame(&frame).expect("decode hello");
            assert_eq!(h.opcode, proto::core_op::HELLO);
            assert_eq!(h.kind, proto::FrameKind::Request);
            assert_eq!(h.req_id, 1);
            assert_eq!(h.seq, 1);
            let mut body = Vec::new();
            proto::Welcome {
                proto_major: proto::PROTOCOL_MAJOR,
                proto_minor: proto::PROTOCOL_MINOR,
                features: 0,
                max_frame_body: proto::DEFAULT_MAX_FRAME_BODY,
                session_id,
            }
            .encode(&mut body);
            // The handshake reply carries the WELCOME opcode, not HELLO.
            let welcome = proto::FrameHeader {
                version: proto::PROTOCOL_MAJOR,
                flags: 0,
                kind: proto::FrameKind::Response,
                opcode: proto::core_op::WELCOME,
                req_id: h.req_id,
                seq: h.seq,
                body_len: body.len() as u32,
            };
            server
                .send(&proto::encode_frame(&welcome, &body))
                .expect("responder send welcome");

            // 2. Noop: first post-handshake call, so req_id/seq == 2.
            let frame = server.recv().expect("responder recv noop");
            let (h, _) = proto::decode_frame(&frame).expect("decode noop");
            assert_eq!(h.opcode, proto::core_op::NOOP);
            assert_eq!(h.req_id, 2);
            assert_eq!(h.seq, 2);
            server
                .send(&response_frame(&h, &[]))
                .expect("responder send noop ack");

            // 3. CreateInstance: counters advanced to 3.
            let frame = server.recv().expect("responder recv create_instance");
            let (h, b) = proto::decode_frame(&frame).expect("decode create_instance");
            assert_eq!(h.opcode, proto::vk_op::CREATE_INSTANCE);
            assert_eq!(h.req_id, 3);
            assert_eq!(h.seq, 3);
            let req = proto::vk::CreateInstanceRequest::decode(b).expect("decode req body");
            assert_eq!(req.app_api_version, 0x0040_3000);
            let mut body = Vec::new();
            proto::vk::CreateInstanceResponse { instance }.encode(&mut body);
            server
                .send(&response_frame(&h, &body))
                .expect("responder send instance");

            // 4. EnumeratePhysicalDevices: counters advanced to 4.
            let frame = server.recv().expect("responder recv enumerate");
            let (h, b) = proto::decode_frame(&frame).expect("decode enumerate");
            assert_eq!(h.opcode, proto::vk_op::ENUMERATE_PHYSICAL_DEVICES);
            assert_eq!(h.req_id, 4);
            assert_eq!(h.seq, 4);
            let req =
                proto::vk::EnumeratePhysicalDevicesRequest::decode(b).expect("decode req body");
            assert_eq!(req.instance, instance);
            let mut body = Vec::new();
            proto::vk::EnumeratePhysicalDevicesResponse {
                devices: vec![device],
            }
            .encode(&mut body);
            server
                .send(&response_frame(&h, &body))
                .expect("responder send devices");
        });

        let mut client = Client::connect(client_t).expect("connect");
        assert_eq!(client.session_id(), session_id);

        client.noop().expect("noop");

        let got_instance = client
            .vk_create_instance(0x0040_3000)
            .expect("create_instance");
        assert_eq!(got_instance, instance);

        let got_devices = client
            .vk_enumerate_physical_devices(got_instance)
            .expect("enumerate_physical_devices");
        assert_eq!(got_devices, vec![device]);

        responder.join().expect("responder thread");
    }
}
