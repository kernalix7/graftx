//! End-to-end Vulkan test: the client completes the handshake, creates a
//! `VkInstance`, then issues `vkEnumeratePhysicalDevices` against a server whose
//! session has a registered [`VulkanBackend`]. The stub backend mints exactly
//! one physical device per enumerate call.

use std::thread;

use graftx_server::{Session, VulkanBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_create_instance_then_enumerate_physical_devices() {
    let (mut client, mut server) = loopback();

    // Server: register the Vulkan backend, then handle exactly three requests
    // (the handshake Hello, the create-instance, then the enumerate), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00CA_FE00);
        session.register(Box::new(VulkanBackend::new()));
        for _ in 0..3 {
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
    // The single device must name a `VkPhysicalDevice` (server kind 2).
    assert_eq!(devices[0].kind(), 2);

    server_thread.join().expect("join server");
}
