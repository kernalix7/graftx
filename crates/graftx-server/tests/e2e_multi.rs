//! End-to-end multi-backend routing test: a single [`Session`] has the Vulkan,
//! OpenGL, CUDA, and WebGPU backends registered at once. Over one loopback
//! client the test completes the handshake, then issues one create/request call
//! per API. Each reply must be routed to the correct backend, which is checked
//! by asserting the server object kind of every returned handle (Vulkan
//! instance 1, GL context 10, CUDA context 20, WebGPU device 70).

use std::thread;

use graftx_server::{CudaBackend, GlBackend, Session, VulkanBackend, WebGpuBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn one_session_routes_each_api_to_its_backend() {
    let (mut client, mut server) = loopback();

    // Server: register all four backends on one session, then handle exactly
    // five requests (the handshake Hello plus one call per API), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00_4D_55_4C);
        session.register(Box::new(VulkanBackend::new()));
        session.register(Box::new(GlBackend::new()));
        session.register(Box::new(CudaBackend::new()));
        session.register(Box::new(WebGpuBackend::new()));
        for _ in 0..5 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00_4D_55_4C);

    // Vulkan: create an instance -> server kind 1 (`VkInstance`).
    let instance = graftx_client::vk::create_instance(&mut client, 0, 2, 2).expect("vk instance");
    assert_eq!(instance.kind(), 1);

    // OpenGL: create a context -> server kind 10 (`GlContext`).
    let gl_ctx = graftx_client::gl::create_context(&mut client, 3, 3).expect("gl context");
    assert_eq!(gl_ctx.kind(), 10);

    // CUDA: create a context on device 0 -> server kind 20 (`CudaContext`).
    let cuda_ctx = graftx_client::cuda::ctx_create(&mut client, 0, 4, 4).expect("cuda ctx");
    assert_eq!(cuda_ctx.kind(), 20);

    // WebGPU: request a device -> server kind 70 (`WgpuDevice`).
    let wgpu_dev = graftx_client::wgpu::request_device(&mut client, 5, 5).expect("wgpu device");
    assert_eq!(wgpu_dev.kind(), 70);

    server_thread.join().expect("join server");
}
