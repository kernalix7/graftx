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
/// Server object kind for a `VkDevice` handle.
const KIND_DEVICE: u8 = 3;
/// Server object kind for a `VkQueue` handle.
const KIND_QUEUE: u8 = 4;

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

/// Server-side state tracked for one created `VkDevice`.
#[derive(Debug)]
struct DeviceState {
    /// The physical device this logical device was created from. Recorded for
    /// lifetime/ownership checks; read only by tests for now.
    #[allow(dead_code)]
    physical_device: proto::Handle,
}

/// Server-side state tracked for one retrieved `VkQueue`.
#[derive(Debug)]
struct QueueState {
    /// The logical device this queue belongs to. Recorded for
    /// lifetime/ownership checks; read only by tests for now.
    #[allow(dead_code)]
    device: proto::Handle,
    /// Index of the queue family this queue was requested from.
    #[allow(dead_code)]
    family: u32,
    /// Index of the queue within its family.
    #[allow(dead_code)]
    index: u32,
}

/// Pure-Rust Vulkan backend stub.
///
/// Owns generational handle tables for the Vulkan objects it tracks. It answers
/// [`vk_op::CREATE_INSTANCE`](proto::vk_op::CREATE_INSTANCE) by minting an
/// instance handle and [`vk_op::ENUMERATE_PHYSICAL_DEVICES`](proto::vk_op::ENUMERATE_PHYSICAL_DEVICES)
/// by minting a single physical-device handle parented to a previously created
/// instance. It also answers [`vk_op::CREATE_DEVICE`](proto::vk_op::CREATE_DEVICE)
/// by minting a logical-device handle parented to a known physical device and
/// [`vk_op::GET_DEVICE_QUEUE`](proto::vk_op::GET_DEVICE_QUEUE) by minting a queue
/// handle parented to a known logical device. The real driver bridge lands in a
/// later milestone; every other Vulkan call is reported as not-yet-implemented
/// via [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct VulkanBackend {
    instances: HandleTable<InstanceState>,
    physical_devices: HandleTable<PhysDevState>,
    devices: HandleTable<DeviceState>,
    queues: HandleTable<QueueState>,
}

impl VulkanBackend {
    /// Create a new Vulkan backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            instances: HandleTable::new(),
            physical_devices: HandleTable::new(),
            devices: HandleTable::new(),
            queues: HandleTable::new(),
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
            proto::vk_op::CREATE_DEVICE => {
                let req = proto::vk::CreateDeviceRequest::decode(body)?;
                // The physical device must have been minted by this backend.
                if self.physical_devices.get(req.physical_device).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let device = self.devices.insert(
                    KIND_DEVICE,
                    DeviceState {
                        physical_device: req.physical_device,
                    },
                );
                let mut out = Vec::new();
                proto::vk::CreateDeviceResponse { device }.encode(&mut out);
                Ok(out)
            }
            proto::vk_op::GET_DEVICE_QUEUE => {
                let req = proto::vk::GetDeviceQueueRequest::decode(body)?;
                // The logical device must have been created on this backend.
                if self.devices.get(req.device).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let queue = self.queues.insert(
                    KIND_QUEUE,
                    QueueState {
                        device: req.device,
                        family: req.queue_family_index,
                        index: req.queue_index,
                    },
                );
                let mut out = Vec::new();
                proto::vk::GetDeviceQueueResponse { queue }.encode(&mut out);
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

    /// Encode a `CREATE_DEVICE` request body for `physical_device`.
    fn create_device_body(physical_device: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::CreateDeviceRequest { physical_device }.encode(&mut body);
        body
    }

    /// Encode a `GET_DEVICE_QUEUE` request body.
    fn get_device_queue_body(
        device: proto::Handle,
        queue_family_index: u32,
        queue_index: u32,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::GetDeviceQueueRequest {
            device,
            queue_family_index,
            queue_index,
        }
        .encode(&mut body);
        body
    }

    /// Drive a backend through `CREATE_INSTANCE` then `ENUMERATE_PHYSICAL_DEVICES`,
    /// returning the minted physical-device handle.
    fn enumerate_one(backend: &mut VulkanBackend) -> proto::Handle {
        let create_resp = backend
            .handle(proto::vk_op::CREATE_INSTANCE, 1, &create_instance_body(0))
            .expect("create instance should succeed");
        let instance = proto::vk::CreateInstanceResponse::decode(&create_resp)
            .expect("decode create response")
            .instance;
        let enum_resp = backend
            .handle(
                proto::vk_op::ENUMERATE_PHYSICAL_DEVICES,
                2,
                &enumerate_body(instance),
            )
            .expect("enumerate should succeed");
        proto::vk::EnumeratePhysicalDevicesResponse::decode(&enum_resp)
            .expect("decode enumerate response")
            .devices[0]
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

    #[test]
    fn create_device_after_enumerate_returns_device_handle() {
        let mut backend = VulkanBackend::new();
        let physical_device = enumerate_one(&mut backend);

        let resp = backend
            .handle(
                proto::vk_op::CREATE_DEVICE,
                3,
                &create_device_body(physical_device),
            )
            .expect("create device should succeed");

        let decoded = proto::vk::CreateDeviceResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.device.kind(), KIND_DEVICE);
        assert!(backend.devices.get(decoded.device).is_some());
        let state = backend
            .devices
            .get(decoded.device)
            .expect("device state present");
        assert_eq!(state.physical_device.raw(), physical_device.raw());
    }

    #[test]
    fn create_device_with_bogus_physical_device_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never minted by this backend.
        let bogus = proto::Handle::new(KIND_PHYSICAL_DEVICE, 0, 999);
        let err = backend
            .handle(proto::vk_op::CREATE_DEVICE, 1, &create_device_body(bogus))
            .expect_err("create device with bogus physical device must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn get_device_queue_after_create_device_returns_queue_handle() {
        let mut backend = VulkanBackend::new();
        let physical_device = enumerate_one(&mut backend);
        let create_resp = backend
            .handle(
                proto::vk_op::CREATE_DEVICE,
                3,
                &create_device_body(physical_device),
            )
            .expect("create device should succeed");
        let device = proto::vk::CreateDeviceResponse::decode(&create_resp)
            .expect("decode create device response")
            .device;

        let resp = backend
            .handle(
                proto::vk_op::GET_DEVICE_QUEUE,
                4,
                &get_device_queue_body(device, 7, 2),
            )
            .expect("get device queue should succeed");

        let decoded = proto::vk::GetDeviceQueueResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.queue.kind(), KIND_QUEUE);
        let state = backend
            .queues
            .get(decoded.queue)
            .expect("queue state present");
        assert_eq!(state.device.raw(), device.raw());
        assert_eq!(state.family, 7);
        assert_eq!(state.index, 2);
    }

    #[test]
    fn get_device_queue_with_bogus_device_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_DEVICE, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::GET_DEVICE_QUEUE,
                1,
                &get_device_queue_body(bogus, 0, 0),
            )
            .expect_err("get device queue with bogus device must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }
}
