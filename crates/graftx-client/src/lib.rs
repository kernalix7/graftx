//! GraftX client.
//!
//! Loaded into a Linux-guest application in place of the real GPU driver
//! libraries. Each shim intercepts an API's C entry points, serializes the
//! calls with [`graftx_protocol`], and forwards them over a
//! [`graftx_transport::Transport`] to the Windows-guest server.
#![forbid(unsafe_op_in_unsafe_fn)]

use graftx_protocol::PROTOCOL_VERSION;

/// Protocol version this client speaks; checked against the server during the
/// handshake.
pub fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaks_known_protocol() {
        assert_eq!(protocol_version(), graftx_protocol::PROTOCOL_VERSION);
    }
}
