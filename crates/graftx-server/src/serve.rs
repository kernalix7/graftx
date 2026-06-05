//! Transport-driven serve loop.
//!
//! Drives a [`Session`](crate::Session) over any [`Transport`]: receive a
//! frame, hand it to the session, send back the reply. A clean peer close
//! (`UnexpectedEof`/`BrokenPipe`) ends the loop without error; any other
//! transport failure propagates. [`serve_tcp`] layers this over a
//! [`std::net::TcpListener`], framing each accepted socket with
//! [`StreamTransport`](graftx_transport::StreamTransport) and serving it with a
//! fresh session.

use std::io;
use std::net::{TcpListener, TcpStream};

use graftx_transport::{StreamTransport, Transport};

use crate::Session;

/// Serve one session over a transport until the peer closes or an error occurs.
///
/// Each iteration receives one frame and dispatches it to
/// [`Session::handle`]. On a successful handle the reply is sent back. If the
/// session reports a [`ProtocolError`](graftx_protocol::ProtocolError) the
/// session is stopped cleanly (`Ok(())`); a later milestone will instead send
/// an error frame to the peer. A `recv` that fails with `UnexpectedEof` or
/// `BrokenPipe` is treated as a clean peer close and returns `Ok(())`; any
/// other `recv`/`send` error propagates.
pub fn serve<T: Transport>(transport: &mut T, session: &mut Session) -> io::Result<()> {
    loop {
        let frame = match transport.recv() {
            Ok(frame) => frame,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(());
            }
            Err(err) => return Err(err),
        };

        match session.handle(&frame) {
            Ok(reply) => transport.send(&reply)?,
            // A protocol-level error ends the session cleanly for now; a future
            // milestone will send a structured error frame before stopping.
            Err(_protocol_error) => return Ok(()),
        }
    }
}

/// Accept TCP connections on `addr` and serve each with a fresh session.
///
/// Binds a [`TcpListener`], then for every accepted [`TcpStream`] wraps it in a
/// [`StreamTransport`] and runs [`serve`] with a session produced by
/// `make_session`. A serve error on one connection is logged and ignored so the
/// listener keeps accepting subsequent connections; this function only returns
/// if accepting itself fails.
pub fn serve_tcp<F: FnMut() -> Session>(addr: &str, mut make_session: F) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        let stream: TcpStream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("graftx-server: accept failed: {err}");
                continue;
            }
        };
        let mut transport = StreamTransport::new(stream);
        let mut session = make_session();
        if let Err(err) = serve(&mut transport, &mut session) {
            eprintln!("graftx-server: connection serve error: {err}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    use graftx_transport::loopback;

    #[test]
    fn serve_handles_frames_then_returns_on_peer_drop() {
        let (mut server_side, mut client_side) = loopback();

        let server = thread::spawn(move || {
            let mut session = Session::new(0x51);
            session.register(Box::new(crate::VulkanBackend::new()));
            serve(&mut server_side, &mut session)
        });

        // Client: handshake, then a noop round-trip against the served session.
        let welcome = graftx_client::handshake(&mut client_side).expect("handshake");
        assert_eq!(welcome.session_id, 0x51);
        graftx_client::noop(&mut client_side, 2, 2).expect("noop");

        // Dropping the client closes the loopback, so serve must return Ok(()).
        drop(client_side);
        let result = server.join().expect("server thread");
        assert!(result.is_ok());
    }
}
