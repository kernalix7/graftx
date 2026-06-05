//! GraftX server library.
//!
//! The server runs on the Windows guest that owns the physical GPU. It decodes
//! the forwarded command stream, validates each command (the stream is
//! untrusted), and replays it against the native driver. This crate exposes the
//! per-session state machine; the binary in `main.rs` wires it to a transport.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod session;

pub use backend::{Backend, VulkanBackend};
pub use session::Session;
