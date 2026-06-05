//! Vulkan client shim (Rust-level).
//!
//! Serializes Vulkan entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's Vulkan backend.
//! These are plain Rust functions; C-ABI export of the `vk*` entry points comes
//! later. This is a pure-Rust shim — no `ash`, no Vulkan SDK, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

/// Issue `vkEnumeratePhysicalDevices` and return the device count.
///
/// Sends an `ENUMERATE_PHYSICAL_DEVICES` request with an empty body, awaits the
/// correlated response, validates its opcode and kind, and parses the 4-byte
/// little-endian `u32` device count from the response body.
pub fn enumerate_physical_devices<T: Transport>(
    t: &mut T,
    req_id: u32,
    seq: u64,
) -> Result<u32, ClientError> {
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::ENUMERATE_PHYSICAL_DEVICES,
        req_id,
        seq,
        body_len: 0,
    };
    t.send(&proto::encode_frame(&header, &[]))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::ENUMERATE_PHYSICAL_DEVICES || h.kind != proto::FrameKind::Response
    {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let count_bytes: [u8; 4] = b
        .get(..4)
        .and_then(|s| s.try_into().ok())
        .ok_or(proto::ProtocolError::UnexpectedEof)?;
    Ok(u32::from_le_bytes(count_bytes))
}
