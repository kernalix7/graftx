//! End-to-end test for the OptiX, SYCL, and AMF backends on one [`Session`].
//!
//! A single session has all three backends registered at once. Over one
//! loopback client the test completes the handshake, then exercises each API in
//! turn: OptiX (context -> pipeline -> destroy), SYCL (queue -> malloc -> free),
//! and AMF (create encoder -> encode frame -> destroy encoder). Each reply must
//! be routed to the correct backend, which is checked by asserting the server
//! object kind of every minted handle (OptiX context 80, pipeline 81, SYCL
//! queue 90, device pointer 91, AMF encoder 100). The AMF encode call must echo
//! the submitted frame length back as the produced packet length.

use std::thread;

use graftx_server::{AmfBackend, OptixBackend, Session, SyclBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn one_session_exercises_optix_sycl_amf() {
    let (mut client, mut server) = loopback();

    // Server: register all three backends on one session, then handle exactly
    // ten requests (the handshake Hello plus three calls per API), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00_47_46_58);
        session.register(Box::new(OptixBackend::new()));
        session.register(Box::new(SyclBackend::new()));
        session.register(Box::new(AmfBackend::new()));
        for _ in 0..10 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00_47_46_58);

    // OptiX: create a context -> kind 80, then a pipeline in it -> kind 81,
    // then destroy the pipeline.
    let optix_ctx = graftx_client::optix::context_create(&mut client, 2, 2).expect("optix context");
    assert_eq!(optix_ctx.kind(), 80);
    let optix_pipeline = graftx_client::optix::pipeline_create(&mut client, optix_ctx, 3, 3)
        .expect("optix pipeline");
    assert_eq!(optix_pipeline.kind(), 81);
    graftx_client::optix::destroy(&mut client, optix_pipeline, 4, 4).expect("optix destroy");

    // SYCL: create a queue -> kind 90, allocate device memory on it -> kind 91,
    // then free it.
    let sycl_queue = graftx_client::sycl::queue_create(&mut client, 5, 5).expect("sycl queue");
    assert_eq!(sycl_queue.kind(), 90);
    let sycl_ptr =
        graftx_client::sycl::malloc_device(&mut client, sycl_queue, 4096, 6, 6).expect("sycl ptr");
    assert_eq!(sycl_ptr.kind(), 91);
    graftx_client::sycl::free(&mut client, sycl_ptr, 7, 7).expect("sycl free");

    // AMF: create an encoder -> kind 100, encode one frame (the packet length
    // echoes the submitted frame length), then destroy the encoder.
    let amf_encoder =
        graftx_client::amf::create_encoder(&mut client, 1, 1920, 1080, 8, 8).expect("amf encoder");
    assert_eq!(amf_encoder.kind(), 100);
    let packet_len = graftx_client::amf::encode_frame(&mut client, amf_encoder, 65536, 9, 9)
        .expect("amf encode");
    assert_eq!(packet_len, 65536);
    graftx_client::amf::destroy_encoder(&mut client, amf_encoder, 10, 10).expect("amf destroy");

    server_thread.join().expect("join server");
}
