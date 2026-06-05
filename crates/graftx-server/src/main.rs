//! GraftX server binary.
//!
//! Runs on the Windows guest that owns the physical GPU. It decodes the
//! forwarded command stream, validates each command, and replays it against the
//! native driver. At M0 this entry point only reports build/protocol info; the
//! transport accept loop and backend dispatch land in later milestones (see the
//! Milestones chapter).
#![forbid(unsafe_op_in_unsafe_fn)]

fn main() {
    println!(
        "graftx-server {} — protocol v{}.{}",
        env!("CARGO_PKG_VERSION"),
        graftx_protocol::PROTOCOL_MAJOR,
        graftx_protocol::PROTOCOL_MINOR,
    );
    println!("M0 scaffold: session state machine available; transport accept loop not yet wired.");
}
