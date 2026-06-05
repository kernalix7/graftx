//! Client-only round-trip tests for the OpenGL command shims.
//!
//! Each test drives a real `graftx_client::gl` shim against an in-process
//! [`graftx_transport::loopback`] pair. A tiny inline responder thread decodes
//! the request frame, builds a canned `Response` with the [`graftx_protocol`]
//! codecs, and sends it back. This exercises the encode/send/recv/validate/decode
//! path of the shims without any dependency on `graftx-server`.

use std::thread;

use graftx_client::gl;
use graftx_protocol as proto;
use graftx_transport::{loopback, Transport};

/// Server object kinds the responder works with. These mirror the kinds the
/// real server backend assigns.
const KIND_GL_CONTEXT: u8 = 10;
const KIND_GL_BUFFER: u8 = 11;

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
fn swap_buffers_acknowledges() {
    let (mut client, mut server) = loopback();
    let context = proto::Handle::new(KIND_GL_CONTEXT, 1, 1);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let body = expect_request(&frame, proto::gl_op::SWAP_BUFFERS);
        let req = proto::gl::SwapBuffersRequest::decode(&body).expect("decode req body");
        assert_eq!(req.context, context);
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");

        // SwapBuffers' reply is an empty ack body.
        server
            .send(&response_frame(&h, &[]))
            .expect("responder send");
    });

    gl::swap_buffers(&mut client, context, 20, 1).expect("swap_buffers");
    responder.join().expect("responder thread");
}

#[test]
fn delete_buffer_acknowledges() {
    let (mut client, mut server) = loopback();
    let buffer = proto::Handle::new(KIND_GL_BUFFER, 1, 5);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let body = expect_request(&frame, proto::gl_op::DELETE_BUFFER);
        let req = proto::gl::DeleteBufferRequest::decode(&body).expect("decode req body");
        assert_eq!(req.buffer, buffer);
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");

        // DeleteBuffer's reply is an empty ack body.
        server
            .send(&response_frame(&h, &[]))
            .expect("responder send");
    });

    gl::delete_buffer(&mut client, buffer, 21, 2).expect("delete_buffer");
    responder.join().expect("responder thread");
}

#[test]
fn swap_buffers_rejects_wrong_opcode() {
    let (mut client, mut server) = loopback();
    let context = proto::Handle::new(KIND_GL_CONTEXT, 1, 1);

    let responder = thread::spawn(move || {
        let frame = server.recv().expect("responder recv");
        let (h, _) = proto::decode_frame(&frame).expect("decode request header");
        // Reply with the wrong opcode to exercise the validation path.
        let bogus = proto::FrameHeader {
            version: proto::PROTOCOL_MAJOR,
            flags: 0,
            kind: proto::FrameKind::Response,
            opcode: proto::gl_op::DELETE_BUFFER,
            req_id: h.req_id,
            seq: h.seq,
            body_len: 0,
        };
        server
            .send(&proto::encode_frame(&bogus, &[]))
            .expect("responder send");
    });

    let err = gl::swap_buffers(&mut client, context, 22, 3).expect_err("wrong opcode must error");
    match err {
        graftx_client::ClientError::UnexpectedReply { opcode, kind } => {
            assert_eq!(opcode, proto::gl_op::DELETE_BUFFER);
            assert_eq!(kind, proto::FrameKind::Response);
        }
        other => panic!("expected UnexpectedReply, got {other:?}"),
    }
    responder.join().expect("responder thread");
}
