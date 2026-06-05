//! Per-API backend dispatch.
//!
//! The [`Session`](crate::Session) routes any non-core opcode to the [`Backend`]
//! registered for that opcode's API namespace. A backend receives the decoded
//! opcode, the request correlation id, and the request body; it returns the
//! *response body* bytes, which the session wraps into a `Response` frame.
//!
//! The Vulkan backend here is a pure-Rust **stub**: it answers the calls that do
//! not yet need a real driver and rejects the rest as not-yet-implemented. No
//! Vulkan SDK, GPU, or `ash` dependency is involved.

use graftx_protocol as proto;

/// A handler for one API namespace (one [`ApiId`](proto::ApiId)).
///
/// Implementors validate and replay the calls in their namespace and return the
/// response *body*; the session is responsible for framing (opcode, `req_id`,
/// and the server-stamped `seq`).
pub trait Backend: Send {
    /// The API namespace this backend serves. The session routes opcodes whose
    /// [`opcode_api`](proto::opcode_api) matches this id here.
    fn api(&self) -> proto::ApiId;

    /// Handle one decoded request, returning the response body bytes.
    fn handle(
        &mut self,
        opcode: u32,
        req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError>;
}

/// Pure-Rust Vulkan backend stub.
///
/// Answers [`vk_op::ENUMERATE_PHYSICAL_DEVICES`](proto::vk_op::ENUMERATE_PHYSICAL_DEVICES)
/// with a device count of zero; the real driver bridge lands in a later
/// milestone. Every other Vulkan call is reported as not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Debug, Default)]
pub struct VulkanBackend;

impl VulkanBackend {
    /// Create a new Vulkan backend stub.
    pub fn new() -> Self {
        Self
    }
}

impl Backend for VulkanBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::Vulkan
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        _body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the Vulkan namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Vulkan as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::vk_op::ENUMERATE_PHYSICAL_DEVICES => {
                // Stub: no physical devices yet. Reply with a 4-byte LE count.
                Ok(0u32.to_le_bytes().to_vec())
            }
            // Everything else in the Vulkan namespace is not implemented yet.
            proto::vk_op::CREATE_INSTANCE
            | proto::vk_op::DESTROY_INSTANCE
            | proto::vk_op::GET_PHYSICAL_DEVICE_PROPERTIES
            | proto::vk_op::CREATE_DEVICE
            | proto::vk_op::DESTROY_DEVICE
            | proto::vk_op::GET_DEVICE_QUEUE
            | proto::vk_op::DEVICE_WAIT_IDLE => Err(proto::ProtocolError::UnknownOpcode(opcode)),
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}
