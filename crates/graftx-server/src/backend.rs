//! Per-API backend dispatch.
//!
//! The [`Session`](crate::Session) routes any non-core opcode to the [`Backend`]
//! registered for that opcode's API namespace. A backend receives the decoded
//! opcode, the request correlation id, and the request body; it returns the
//! *response body* bytes, which the session wraps into a `Response` frame.
//!
//! The Vulkan backend here is a pure-Rust **stub**: it tracks object lifetimes
//! in generational handle tables but performs no real driver work. No Vulkan
//! SDK, GPU, or `ash` dependency is involved.

use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a `VkInstance` handle.
const KIND_INSTANCE: u8 = 1;
/// Server object kind for a `VkPhysicalDevice` handle.
const KIND_PHYSICAL_DEVICE: u8 = 2;

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

/// Server-side state tracked for one created `VkInstance`.
#[derive(Debug, Default)]
struct InstanceState {
    /// Marks the instance as live; reserved for future per-instance state.
    created: bool,
}

/// Server-side state tracked for one minted `VkPhysicalDevice`.
#[derive(Debug)]
struct PhysDevState {
    /// The instance handle this physical device was enumerated from. Recorded
    /// for the lifetime/ownership checks added in a later milestone; read only
    /// by tests for now.
    #[allow(dead_code)]
    parent: proto::Handle,
}

/// Pure-Rust Vulkan backend stub.
///
/// Owns generational handle tables for the Vulkan objects it tracks. It answers
/// [`vk_op::CREATE_INSTANCE`](proto::vk_op::CREATE_INSTANCE) by minting an
/// instance handle and [`vk_op::ENUMERATE_PHYSICAL_DEVICES`](proto::vk_op::ENUMERATE_PHYSICAL_DEVICES)
/// by minting a single physical-device handle parented to a previously created
/// instance. The real driver bridge lands in a later milestone; every other
/// Vulkan call is reported as not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct VulkanBackend {
    instances: HandleTable<InstanceState>,
    physical_devices: HandleTable<PhysDevState>,
}

impl VulkanBackend {
    /// Create a new Vulkan backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            instances: HandleTable::new(),
            physical_devices: HandleTable::new(),
        }
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
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the Vulkan namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Vulkan as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::vk_op::CREATE_INSTANCE => {
                let _req = proto::vk::CreateInstanceRequest::decode(body)?;
                let instance = self
                    .instances
                    .insert(KIND_INSTANCE, InstanceState { created: true });
                let mut out = Vec::new();
                proto::vk::CreateInstanceResponse { instance }.encode(&mut out);
                Ok(out)
            }
            proto::vk_op::ENUMERATE_PHYSICAL_DEVICES => {
                let req = proto::vk::EnumeratePhysicalDevicesRequest::decode(body)?;
                // The instance must have been created and still be live on this
                // backend.
                let live = self
                    .instances
                    .get(req.instance)
                    .is_some_and(|state| state.created);
                if !live {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let device = self.physical_devices.insert(
                    KIND_PHYSICAL_DEVICE,
                    PhysDevState {
                        parent: req.instance,
                    },
                );
                let mut out = Vec::new();
                proto::vk::EnumeratePhysicalDevicesResponse {
                    devices: vec![device],
                }
                .encode(&mut out);
                Ok(out)
            }
            // Everything else in the Vulkan namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `CREATE_INSTANCE` request body.
    fn create_instance_body(app_api_version: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::CreateInstanceRequest { app_api_version }.encode(&mut body);
        body
    }

    /// Encode an `ENUMERATE_PHYSICAL_DEVICES` request body for `instance`.
    fn enumerate_body(instance: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::EnumeratePhysicalDevicesRequest { instance }.encode(&mut body);
        body
    }

    #[test]
    fn create_instance_returns_decodable_handle() {
        let mut backend = VulkanBackend::new();
        let body = create_instance_body(0x0040_3000);
        let resp = backend
            .handle(proto::vk_op::CREATE_INSTANCE, 1, &body)
            .expect("create instance should succeed");

        let decoded = proto::vk::CreateInstanceResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.instance.kind(), KIND_INSTANCE);
        // The returned handle must resolve in the instance table.
        assert!(backend.instances.get(decoded.instance).is_some());
    }

    #[test]
    fn enumerate_after_create_returns_one_device() {
        let mut backend = VulkanBackend::new();
        let create_body = create_instance_body(0);
        let create_resp = backend
            .handle(proto::vk_op::CREATE_INSTANCE, 1, &create_body)
            .expect("create instance should succeed");
        let instance = proto::vk::CreateInstanceResponse::decode(&create_resp)
            .expect("decode create response")
            .instance;

        let enum_body = enumerate_body(instance);
        let enum_resp = backend
            .handle(proto::vk_op::ENUMERATE_PHYSICAL_DEVICES, 2, &enum_body)
            .expect("enumerate should succeed");

        let decoded = proto::vk::EnumeratePhysicalDevicesResponse::decode(&enum_resp)
            .expect("decode enumerate response");
        assert_eq!(decoded.devices.len(), 1);
        let device = decoded.devices[0];
        assert_eq!(device.kind(), KIND_PHYSICAL_DEVICE);
        assert!(backend.physical_devices.get(device).is_some());
    }

    #[test]
    fn enumerate_with_bogus_instance_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_INSTANCE, 0, 999);
        let body = enumerate_body(bogus);
        let err = backend
            .handle(proto::vk_op::ENUMERATE_PHYSICAL_DEVICES, 1, &body)
            .expect_err("enumerate with bogus instance must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = VulkanBackend::new();
        // Opcode 0 is a core opcode, not in the Vulkan namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-vulkan opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }

    #[test]
    fn parent_link_is_recorded() {
        let mut backend = VulkanBackend::new();
        let create_body = create_instance_body(0);
        let create_resp = backend
            .handle(proto::vk_op::CREATE_INSTANCE, 1, &create_body)
            .expect("create instance should succeed");
        let instance = proto::vk::CreateInstanceResponse::decode(&create_resp)
            .expect("decode create response")
            .instance;

        let enum_body = enumerate_body(instance);
        let enum_resp = backend
            .handle(proto::vk_op::ENUMERATE_PHYSICAL_DEVICES, 2, &enum_body)
            .expect("enumerate should succeed");
        let device = proto::vk::EnumeratePhysicalDevicesResponse::decode(&enum_resp)
            .expect("decode enumerate response")
            .devices[0];

        let state = backend
            .physical_devices
            .get(device)
            .expect("device state present");
        assert_eq!(state.parent.raw(), instance.raw());
    }
}
