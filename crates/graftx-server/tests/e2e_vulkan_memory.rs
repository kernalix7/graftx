//! End-to-end Vulkan memory test: the client completes the handshake, creates a
//! `VkInstance`, enumerates physical devices, creates a logical device on the
//! first physical device, allocates device memory, creates a buffer, and binds
//! the memory to the buffer — all against a server whose session has a
//! registered [`VulkanBackend`]. Exercises the allocate-memory, create-buffer,
//! and bind-buffer-memory path end to end.

use std::thread;

use graftx_server::{Session, VulkanBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_allocate_memory_create_buffer_then_bind() {
    let (mut client, mut server) = loopback();

    // Server: register the Vulkan backend, then handle exactly seven requests
    // (the handshake Hello, create-instance, enumerate, create-device,
    // allocate-memory, create-buffer, and bind-buffer-memory), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00CA_FE00);
        session.register(Box::new(VulkanBackend::new()));
        for _ in 0..7 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00CA_FE00);

    // Create an instance, then enumerate physical devices on it.
    let instance =
        graftx_client::vk::create_instance(&mut client, 0, 2, 2).expect("create instance");
    let devices = graftx_client::vk::enumerate_physical_devices(&mut client, instance, 3, 3)
        .expect("enumerate physical devices");
    assert_eq!(devices.len(), 1);

    // Create a logical device on the first physical device.
    let device =
        graftx_client::vk::create_device(&mut client, devices[0], 4, 4).expect("create device");

    // Allocate device memory, create a buffer, then bind the memory to it.
    let memory = graftx_client::vk::allocate_memory(&mut client, device, 4096, 5, 5)
        .expect("allocate memory");
    // The allocation must name a `VkDeviceMemory` (server kind 5).
    assert_eq!(memory.kind(), 5);

    let buffer = graftx_client::vk::create_buffer(&mut client, device, 4096, 0, 6, 6)
        .expect("create buffer");
    // The buffer must name a `VkBuffer` (server kind 6).
    assert_eq!(buffer.kind(), 6);

    // Bind the memory to the buffer; the call must return `Ok(())`.
    let bind: Result<(), _> =
        graftx_client::vk::bind_buffer_memory(&mut client, buffer, memory, 0, 7, 7);
    assert!(bind.is_ok());
    bind.expect("bind buffer memory should succeed");

    server_thread.join().expect("join server");
}
