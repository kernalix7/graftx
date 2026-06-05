//! Client-only round-trip tests for the Vulkan command shims.
//!
//! Each test drives a real `graftx_client::vk` shim against an in-process
//! [`graftx_transport::loopback`] pair. A tiny inline responder thread decodes
//! the request frame, builds a canned `Response` with the [`graftx_protocol`]
//! codecs, and sends it back. This exercises the encode/send/recv/validate/decode
//! path of the shims without any dependency on `graftx-server`.

use std::thread;

use graftx_client::vk;
use graftx_protocol as proto;
use graftx_transport::{loopback, Transport};

/// Server object kinds the responder stamps into the handles it returns. These
/// mirror the kinds the real server backend assigns.
const KIND_COMMAND_POOL: u8 = 7;
const KIND_COMMAND_BUFFER: u8 = 8;

/// Decode one request frame, assert its opcode, and hand back the body bytes.
fn expect_request(frame: &[u8], opcode: u32) -> Vec<u8> {
    let (h, b) = proto::decode_frame(frame).expect("decode request frame");
    assert_eq!(h.opcode, opcode, "request opcode");
    assert_eq!(h.kind, proto::FrameKind::Request, "request kind");
    b.to_vec()
}

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

#[test]
fn create_command_pool_parses_returned_handle() {
    let (mut client, mut server) = loopback();
    let pool = proto::Handle::new(KIND_COMMAND_POOL, 1, 42);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let (h, b) = proto::decode_frame(&frame).expect("decode request");
        assert_eq!(h.opcode, proto::vk_op::CREATE_COMMAND_POOL);
        assert_eq!(h.kind, proto::FrameKind::Request);
        let req = proto::vk::CreateCommandPoolRequest::decode(b).expect("decode req body");
        assert_eq!(req.queue_family_index, 3);

        let mut body = Vec::new();
        proto::vk::CreateCommandPoolResponse { pool }.encode(&mut body);
        server
            .send(&response_frame(&h, &body))
            .expect("responder send");
    });

    let device = proto::Handle::new(3, 1, 0);
    let got = vk::create_command_pool(&mut client, device, 3, 7, 1).expect("create_command_pool");
    assert_eq!(got, pool);
    responder.join().expect("responder thread");
}

#[test]
fn allocate_command_buffer_parses_returned_handle() {
    let (mut client, mut server) = loopback();
    let command_buffer = proto::Handle::new(KIND_COMMAND_BUFFER, 1, 99);
    let pool = proto::Handle::new(KIND_COMMAND_POOL, 1, 42);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let body = expect_request(&frame, proto::vk_op::ALLOCATE_COMMAND_BUFFER);
        let req = proto::vk::AllocateCommandBufferRequest::decode(&body).expect("decode req body");
        assert_eq!(req.pool, pool);
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");

        let mut out = Vec::new();
        proto::vk::AllocateCommandBufferResponse { command_buffer }.encode(&mut out);
        server
            .send(&response_frame(&h, &out))
            .expect("responder send");
    });

    let got =
        vk::allocate_command_buffer(&mut client, pool, 8, 2).expect("allocate_command_buffer");
    assert_eq!(got, command_buffer);
    responder.join().expect("responder thread");
}

#[test]
fn queue_submit_acknowledges() {
    let (mut client, mut server) = loopback();
    let queue = proto::Handle::new(4, 1, 0);
    let command_buffer = proto::Handle::new(KIND_COMMAND_BUFFER, 1, 99);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let body = expect_request(&frame, proto::vk_op::QUEUE_SUBMIT);
        let req = proto::vk::QueueSubmitRequest::decode(&body).expect("decode req body");
        assert_eq!(req.queue, queue);
        assert_eq!(req.command_buffer, command_buffer);
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");

        // QueueSubmit's reply is an empty ack body.
        server
            .send(&response_frame(&h, &[]))
            .expect("responder send");
    });

    vk::queue_submit(&mut client, queue, command_buffer, 9, 3).expect("queue_submit");
    responder.join().expect("responder thread");
}

#[test]
fn cmd_copy_buffer_acknowledges() {
    let (mut client, mut server) = loopback();
    let command_buffer = proto::Handle::new(KIND_COMMAND_BUFFER, 1, 99);
    let src = proto::Handle::new(6, 1, 10);
    let dst = proto::Handle::new(6, 1, 11);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let body = expect_request(&frame, proto::vk_op::CMD_COPY_BUFFER);
        let req = proto::vk::CmdCopyBufferRequest::decode(&body).expect("decode req body");
        assert_eq!(req.command_buffer, command_buffer);
        assert_eq!(req.src, src);
        assert_eq!(req.dst, dst);
        assert_eq!(req.size, 256);
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");

        // CmdCopyBuffer's reply is an empty ack body.
        server
            .send(&response_frame(&h, &[]))
            .expect("responder send");
    });

    vk::cmd_copy_buffer(&mut client, command_buffer, src, dst, 256, 11, 4)
        .expect("cmd_copy_buffer");
    responder.join().expect("responder thread");
}

#[test]
fn cmd_draw_acknowledges() {
    let (mut client, mut server) = loopback();
    let command_buffer = proto::Handle::new(KIND_COMMAND_BUFFER, 1, 99);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let body = expect_request(&frame, proto::vk_op::CMD_DRAW);
        let req = proto::vk::CmdDrawRequest::decode(&body).expect("decode req body");
        assert_eq!(req.command_buffer, command_buffer);
        assert_eq!(req.vertex_count, 3);
        assert_eq!(req.instance_count, 1);
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");

        // CmdDraw's reply is an empty ack body.
        server
            .send(&response_frame(&h, &[]))
            .expect("responder send");
    });

    vk::cmd_draw(&mut client, command_buffer, 3, 1, 12, 5).expect("cmd_draw");
    responder.join().expect("responder thread");
}

#[test]
fn cmd_draw_rejects_wrong_opcode() {
    let (mut client, mut server) = loopback();
    let command_buffer = proto::Handle::new(KIND_COMMAND_BUFFER, 1, 99);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");
        // Reply with the wrong opcode to exercise the validation path.
        let bogus = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Response,
            opcode: proto::vk_op::CMD_COPY_BUFFER,
            req_id: h.req_id,
            seq: h.seq,
            body_len: 0,
        };
        server
            .send(&proto::encode_frame(&bogus, &[]))
            .expect("responder send");
    });

    let err = vk::cmd_draw(&mut client, command_buffer, 3, 1, 13, 6)
        .expect_err("wrong opcode must error");
    match err {
        graftx_client::ClientError::UnexpectedReply { opcode, kind } => {
            assert_eq!(opcode, proto::vk_op::CMD_COPY_BUFFER);
            assert_eq!(kind, proto::FrameKind::Response);
        }
        other => panic!("expected UnexpectedReply, got {other:?}"),
    }
    responder.join().expect("responder thread");
}

#[test]
fn create_command_pool_rejects_wrong_kind() {
    let (mut client, mut server) = loopback();
    // A handle whose kind is *not* KIND_COMMAND_POOL must be rejected.
    let bogus = proto::Handle::new(KIND_COMMAND_BUFFER, 1, 1);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");
        let mut body = Vec::new();
        proto::vk::CreateCommandPoolResponse { pool: bogus }.encode(&mut body);
        server
            .send(&response_frame(&h, &body))
            .expect("responder send");
    });

    let device = proto::Handle::new(3, 1, 0);
    let err =
        vk::create_command_pool(&mut client, device, 0, 1, 1).expect_err("wrong kind must error");
    match err {
        graftx_client::ClientError::Protocol(proto::ProtocolError::BadKind(k)) => {
            assert_eq!(k, u16::from(KIND_COMMAND_BUFFER));
        }
        other => panic!("expected BadKind, got {other:?}"),
    }
    responder.join().expect("responder thread");
}
