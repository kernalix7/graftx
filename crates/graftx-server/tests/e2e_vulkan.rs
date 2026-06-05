//! End-to-end Vulkan test: the client completes the handshake, then issues a
//! `vkEnumeratePhysicalDevices` call against a server whose session has a
//! registered [`VulkanBackend`]. The stub backend reports zero devices.

use std::thread;

use graftx_server::{Session, VulkanBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_then_enumerate_physical_devices() {
    let (mut client, mut server) = loopback();

    // Server: register the Vulkan backend, then handle exactly two requests
    // (the handshake Hello, then the Vulkan enumerate), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00CA_FE00);
        session.register(Box::new(VulkanBackend::new()));
        for _ in 0..2 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00CA_FE00);

    // Then enumerate physical devices; the stub backend has none.
    let count = graftx_client::vk::enumerate_physical_devices(&mut client, 2, 2)
        .expect("enumerate physical devices");
    assert_eq!(count, 0);

    server_thread.join().expect("join server");
}
