//! GraftX wire protocol.
//!
//! Defines the command encoding/decoding shared by the Linux client shim and
//! the Windows server. Both sides negotiate [`PROTOCOL_VERSION`] during the
//! handshake before any GPU API call is forwarded.
#![forbid(unsafe_op_in_unsafe_fn)]

/// Protocol version negotiated during the client/server handshake.
///
/// `0` marks the pre-stable wire format; bumped on every breaking change until
/// the format is frozen.
pub const PROTOCOL_VERSION: u32 = 0;

/// Errors raised while encoding or decoding the wire format.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The buffer ended before a full message could be decoded.
    #[error("unexpected end of buffer")]
    UnexpectedEof,
    /// The decoded opcode does not map to any known command.
    #[error("unknown opcode: {0}")]
    UnknownOpcode(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_prestable() {
        assert_eq!(PROTOCOL_VERSION, 0);
    }
}
