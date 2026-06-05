//! GraftX server binary.
//!
//! Runs on the Windows guest that owns the physical GPU. It decodes the
//! forwarded command stream, validates each command, and replays it against the
//! native driver. With `serve <addr>` it binds a TCP listener and serves each
//! connection with a fresh session (Vulkan + GL backends); without arguments it
//! reports build/protocol info and a usage hint.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicU64, Ordering};

use graftx_server::{GlBackend, Session, VulkanBackend};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("serve"), Some(addr)) => {
            println!("graftx-server: listening on {addr}");
            let next_id = AtomicU64::new(1);
            graftx_server::serve_tcp(&addr, || {
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                let mut session = Session::new(id);
                session.register(Box::new(VulkanBackend::new()));
                session.register(Box::new(GlBackend::new()));
                session
            })
        }
        _ => {
            println!(
                "graftx-server {} — protocol v{}.{}",
                env!("CARGO_PKG_VERSION"),
                graftx_protocol::PROTOCOL_MAJOR,
                graftx_protocol::PROTOCOL_MINOR,
            );
            println!(
                "usage: graftx-server serve <addr>   (e.g. graftx-server serve 127.0.0.1:7000)"
            );
            Ok(())
        }
    }
}
