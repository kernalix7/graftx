//! End-to-end test over a real TCP loopback socket.
//!
//! A `TcpListener` binds an ephemeral `127.0.0.1` port; a server thread accepts
//! exactly one connection, frames it with a [`StreamTransport`], and serves a
//! session with a registered [`VulkanBackend`] via [`graftx_server::serve`]
//! (which returns once the client disconnects). The main thread connects,
//! completes the handshake, creates an instance, and enumerates physical
//! devices, asserting the negotiated protocol version and the single stub
//! device. Dropping the client stream ends the serve loop; the server thread
//! must join with `Ok`.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::thread;

use graftx_protocol as proto;
use graftx_server::{Session, VulkanBackend};
use graftx_transport::StreamTransport;

#[test]
fn tcp_loopback_handshake_create_instance_enumerate() {
    let session_id: u64 = 0x00CA_FE00;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("local addr");

    // Server: accept one connection, frame it, and serve a fresh session with
    // a registered Vulkan backend until the client disconnects.
    let server_thread = thread::spawn(move || -> io::Result<()> {
        let (stream, _peer) = listener.accept().expect("server accept");
        let mut transport = StreamTransport::new(stream);
        let mut session = Session::new(session_id);
        session.register(Box::new(VulkanBackend::new()));
        graftx_server::serve(&mut transport, &mut session)
    });

    // Client: connect over TCP and frame the stream the same way.
    let client_stream = TcpStream::connect(addr).expect("client connect");
    let mut client = StreamTransport::new(client_stream);

    // Handshake: the server must echo the protocol version and our session id.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.proto_major, proto::PROTOCOL_MAJOR);
    assert_eq!(welcome.proto_minor, proto::PROTOCOL_MINOR);
    assert_eq!(welcome.session_id, session_id);

    // Create an instance, then enumerate physical devices on it: the stub
    // backend mints exactly one physical device.
    let instance =
        graftx_client::vk::create_instance(&mut client, 0, 2, 2).expect("create instance");
    let devices = graftx_client::vk::enumerate_physical_devices(&mut client, instance, 3, 3)
        .expect("enumerate physical devices");
    assert_eq!(devices.len(), 1);

    // Drop the client stream to close the connection, ending the serve loop.
    drop(client);
    let result = server_thread.join().expect("join server thread");
    assert!(result.is_ok(), "server serve returned an error: {result:?}");
}
