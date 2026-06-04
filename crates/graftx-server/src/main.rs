//! GraftX server.
//!
//! Runs on the Windows guest that owns the physical GPU (via passthrough).
//! Decodes the forwarded command stream with [`graftx_protocol`], replays each
//! call against the native driver, and returns results over the transport.
#![forbid(unsafe_op_in_unsafe_fn)]

fn main() {
    println!(
        "graftx-server: speaking protocol v{}",
        graftx_protocol::PROTOCOL_VERSION
    );
}
