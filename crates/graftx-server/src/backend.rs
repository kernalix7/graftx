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
/// Server object kind for a `VkDeviceMemory` handle.
const KIND_DEVICE_MEMORY: u8 = 5;
/// Server object kind for a `VkBuffer` handle.
const KIND_BUFFER: u8 = 6;
/// Server object kind for a `VkCommandPool` handle.
const KIND_COMMAND_POOL: u8 = 7;
/// Server object kind for a `VkCommandBuffer` handle.
const KIND_COMMAND_BUFFER: u8 = 8;

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

/// Server-side state tracked for one allocated `VkDeviceMemory`.
#[derive(Debug)]
struct MemoryState {
    /// The logical device this memory was allocated on. Recorded for
    /// lifetime/ownership checks; read only by tests for now.
    #[allow(dead_code)]
    device: proto::Handle,
    /// Size of the allocation in bytes.
    #[allow(dead_code)]
    size: u64,
}

/// Server-side state tracked for one created `VkBuffer`.
#[derive(Debug)]
struct BufferState {
    /// The logical device this buffer was created on. Recorded for
    /// lifetime/ownership checks; read only by tests for now.
    #[allow(dead_code)]
    device: proto::Handle,
    /// Size of the buffer in bytes.
    #[allow(dead_code)]
    size: u64,
    /// Buffer usage flag bits.
    #[allow(dead_code)]
    usage: u32,
    /// The memory binding for this buffer, if one has been recorded: the bound
    /// memory handle and the offset into it. `None` until `BIND_BUFFER_MEMORY`
    /// succeeds; a second bind is rejected.
    bound: Option<(proto::Handle, u64)>,
}

/// Server-side state tracked for one created `VkCommandPool`.
#[derive(Debug)]
struct CommandPoolState {
    /// The logical device this command pool was created on. Recorded for
    /// lifetime/ownership checks; read only by tests for now.
    #[allow(dead_code)]
    device: proto::Handle,
    /// Index of the queue family this pool's buffers are submitted to.
    #[allow(dead_code)]
    queue_family_index: u32,
}

/// Server-side state tracked for one allocated `VkCommandBuffer`.
#[derive(Debug)]
struct CommandBufferState {
    /// The command pool this buffer was allocated from. Recorded for
    /// lifetime/ownership checks; read only by tests for now.
    #[allow(dead_code)]
    pool: proto::Handle,
    /// Number of commands recorded into this buffer. Bumped by each
    /// `CMD_*` opcode that targets the buffer.
    recorded: u32,
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
/// handle parented to a known logical device. It also answers the teardown
/// opcodes [`vk_op::DESTROY_BUFFER`](proto::vk_op::DESTROY_BUFFER),
/// [`vk_op::FREE_MEMORY`](proto::vk_op::FREE_MEMORY), and
/// [`vk_op::DESTROY_COMMAND_POOL`](proto::vk_op::DESTROY_COMMAND_POOL) by
/// removing the named object from its table and acknowledging. The real driver
/// bridge lands in a later milestone; every other Vulkan call is reported as
/// not-yet-implemented via [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct VulkanBackend {
    instances: HandleTable<InstanceState>,
    physical_devices: HandleTable<PhysDevState>,
    devices: HandleTable<DeviceState>,
    queues: HandleTable<QueueState>,
    memories: HandleTable<MemoryState>,
    buffers: HandleTable<BufferState>,
    command_pools: HandleTable<CommandPoolState>,
    command_buffers: HandleTable<CommandBufferState>,
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
            memories: HandleTable::new(),
            buffers: HandleTable::new(),
            command_pools: HandleTable::new(),
            command_buffers: HandleTable::new(),
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
            proto::vk_op::ALLOCATE_MEMORY => {
                let req = proto::vk::AllocateMemoryRequest::decode(body)?;
                // The logical device must have been created on this backend.
                if self.devices.get(req.device).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let memory = self.memories.insert(
                    KIND_DEVICE_MEMORY,
                    MemoryState {
                        device: req.device,
                        size: req.size,
                    },
                );
                let mut out = Vec::new();
                proto::vk::AllocateMemoryResponse { memory }.encode(&mut out);
                Ok(out)
            }
            proto::vk_op::CREATE_BUFFER => {
                let req = proto::vk::CreateBufferRequest::decode(body)?;
                // The logical device must have been created on this backend.
                if self.devices.get(req.device).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let buffer = self.buffers.insert(
                    KIND_BUFFER,
                    BufferState {
                        device: req.device,
                        size: req.size,
                        usage: req.usage,
                        bound: None,
                    },
                );
                let mut out = Vec::new();
                proto::vk::CreateBufferResponse { buffer }.encode(&mut out);
                Ok(out)
            }
            proto::vk_op::BIND_BUFFER_MEMORY => {
                let req = proto::vk::BindBufferMemoryRequest::decode(body)?;
                // The memory must have been allocated on this backend.
                if self.memories.get(req.memory).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // The buffer must exist and not already be bound; a re-bind is
                // rejected.
                let state = self
                    .buffers
                    .get_mut(req.buffer)
                    .filter(|state| state.bound.is_none())
                    .ok_or(proto::ProtocolError::UnknownOpcode(opcode))?;
                state.bound = Some((req.memory, req.offset));
                Ok(Vec::new())
            }
            proto::vk_op::CREATE_COMMAND_POOL => {
                let req = proto::vk::CreateCommandPoolRequest::decode(body)?;
                // The logical device must have been created on this backend.
                if self.devices.get(req.device).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let pool = self.command_pools.insert(
                    KIND_COMMAND_POOL,
                    CommandPoolState {
                        device: req.device,
                        queue_family_index: req.queue_family_index,
                    },
                );
                let mut out = Vec::new();
                proto::vk::CreateCommandPoolResponse { pool }.encode(&mut out);
                Ok(out)
            }
            proto::vk_op::ALLOCATE_COMMAND_BUFFER => {
                let req = proto::vk::AllocateCommandBufferRequest::decode(body)?;
                // The command pool must have been created on this backend.
                if self.command_pools.get(req.pool).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let command_buffer = self.command_buffers.insert(
                    KIND_COMMAND_BUFFER,
                    CommandBufferState {
                        pool: req.pool,
                        recorded: 0,
                    },
                );
                let mut out = Vec::new();
                proto::vk::AllocateCommandBufferResponse { command_buffer }.encode(&mut out);
                Ok(out)
            }
            proto::vk_op::QUEUE_SUBMIT => {
                let req = proto::vk::QueueSubmitRequest::decode(body)?;
                // Both the queue and the command buffer must exist on this
                // backend.
                if self.queues.get(req.queue).is_none()
                    || self.command_buffers.get(req.command_buffer).is_none()
                {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                Ok(Vec::new())
            }
            proto::vk_op::CMD_COPY_BUFFER => {
                let req = proto::vk::CmdCopyBufferRequest::decode(body)?;
                // Both buffers must exist on this backend before the copy can be
                // recorded into the command buffer.
                if self.buffers.get(req.src).is_none() || self.buffers.get(req.dst).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // The command buffer must exist; bump its recorded count.
                let state = self
                    .command_buffers
                    .get_mut(req.command_buffer)
                    .ok_or(proto::ProtocolError::UnknownOpcode(opcode))?;
                state.recorded = state.recorded.saturating_add(1);
                Ok(Vec::new())
            }
            proto::vk_op::CMD_DRAW => {
                let req = proto::vk::CmdDrawRequest::decode(body)?;
                // The command buffer must exist; bump its recorded count.
                let state = self
                    .command_buffers
                    .get_mut(req.command_buffer)
                    .ok_or(proto::ProtocolError::UnknownOpcode(opcode))?;
                state.recorded = state.recorded.saturating_add(1);
                Ok(Vec::new())
            }
            proto::vk_op::DESTROY_BUFFER => {
                let req = proto::vk::DestroyBufferRequest::decode(body)?;
                // The buffer must have been created on this backend; remove it.
                if self.buffers.remove(req.buffer).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                Ok(Vec::new())
            }
            proto::vk_op::FREE_MEMORY => {
                let req = proto::vk::FreeMemoryRequest::decode(body)?;
                // The memory must have been allocated on this backend; free it.
                if self.memories.remove(req.memory).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                Ok(Vec::new())
            }
            proto::vk_op::DESTROY_COMMAND_POOL => {
                let req = proto::vk::DestroyCommandPoolRequest::decode(body)?;
                // The command pool must have been created on this backend; remove it.
                if self.command_pools.remove(req.pool).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                Ok(Vec::new())
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

    /// Encode an `ALLOCATE_MEMORY` request body.
    fn allocate_memory_body(device: proto::Handle, size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::AllocateMemoryRequest { device, size }.encode(&mut body);
        body
    }

    /// Encode a `CREATE_BUFFER` request body.
    fn create_buffer_body(device: proto::Handle, size: u64, usage: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::CreateBufferRequest {
            device,
            size,
            usage,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `BIND_BUFFER_MEMORY` request body.
    fn bind_buffer_memory_body(
        buffer: proto::Handle,
        memory: proto::Handle,
        offset: u64,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::BindBufferMemoryRequest {
            buffer,
            memory,
            offset,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `CREATE_COMMAND_POOL` request body.
    fn create_command_pool_body(device: proto::Handle, queue_family_index: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::CreateCommandPoolRequest {
            device,
            queue_family_index,
        }
        .encode(&mut body);
        body
    }

    /// Encode an `ALLOCATE_COMMAND_BUFFER` request body.
    fn allocate_command_buffer_body(pool: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::AllocateCommandBufferRequest { pool }.encode(&mut body);
        body
    }

    /// Encode a `QUEUE_SUBMIT` request body.
    fn queue_submit_body(queue: proto::Handle, command_buffer: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::QueueSubmitRequest {
            queue,
            command_buffer,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `CMD_COPY_BUFFER` request body.
    fn cmd_copy_buffer_body(
        command_buffer: proto::Handle,
        src: proto::Handle,
        dst: proto::Handle,
        size: u64,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::CmdCopyBufferRequest {
            command_buffer,
            src,
            dst,
            size,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `DESTROY_BUFFER` request body.
    fn destroy_buffer_body(buffer: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::DestroyBufferRequest { buffer }.encode(&mut body);
        body
    }

    /// Encode a `FREE_MEMORY` request body.
    fn free_memory_body(memory: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::FreeMemoryRequest { memory }.encode(&mut body);
        body
    }

    /// Encode a `DESTROY_COMMAND_POOL` request body.
    fn destroy_command_pool_body(pool: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::DestroyCommandPoolRequest { pool }.encode(&mut body);
        body
    }

    /// Encode a `CMD_DRAW` request body.
    fn cmd_draw_body(
        command_buffer: proto::Handle,
        vertex_count: u32,
        instance_count: u32,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        proto::vk::CmdDrawRequest {
            command_buffer,
            vertex_count,
            instance_count,
        }
        .encode(&mut body);
        body
    }

    /// Drive a backend up through `GET_DEVICE_QUEUE`, returning the logical
    /// device handle alongside its minted queue handle.
    fn create_queue_one(backend: &mut VulkanBackend) -> (proto::Handle, proto::Handle) {
        let device = create_device_one(backend);
        let queue_resp = backend
            .handle(
                proto::vk_op::GET_DEVICE_QUEUE,
                4,
                &get_device_queue_body(device, 0, 0),
            )
            .expect("get device queue should succeed");
        let queue = proto::vk::GetDeviceQueueResponse::decode(&queue_resp)
            .expect("decode queue response")
            .queue;
        (device, queue)
    }

    /// Drive a backend up through `CREATE_COMMAND_POOL`, returning the logical
    /// device handle alongside its minted command-pool handle.
    fn create_command_pool_one(backend: &mut VulkanBackend) -> (proto::Handle, proto::Handle) {
        let device = create_device_one(backend);
        let pool_resp = backend
            .handle(
                proto::vk_op::CREATE_COMMAND_POOL,
                7,
                &create_command_pool_body(device, 0),
            )
            .expect("create command pool should succeed");
        let pool = proto::vk::CreateCommandPoolResponse::decode(&pool_resp)
            .expect("decode command pool response")
            .pool;
        (device, pool)
    }

    /// Drive a backend up through `ALLOCATE_COMMAND_BUFFER`, returning the
    /// logical device handle alongside its minted command-buffer handle.
    fn allocate_command_buffer_one(backend: &mut VulkanBackend) -> (proto::Handle, proto::Handle) {
        let (device, pool) = create_command_pool_one(backend);
        let buf_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_COMMAND_BUFFER,
                8,
                &allocate_command_buffer_body(pool),
            )
            .expect("allocate command buffer should succeed");
        let command_buffer = proto::vk::AllocateCommandBufferResponse::decode(&buf_resp)
            .expect("decode command buffer response")
            .command_buffer;
        (device, command_buffer)
    }

    /// Create a buffer on `device`, returning its minted handle.
    fn create_buffer_one(backend: &mut VulkanBackend, device: proto::Handle) -> proto::Handle {
        let buf_resp = backend
            .handle(
                proto::vk_op::CREATE_BUFFER,
                6,
                &create_buffer_body(device, 1024, 0x20),
            )
            .expect("create buffer should succeed");
        proto::vk::CreateBufferResponse::decode(&buf_resp)
            .expect("decode buffer response")
            .buffer
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

    /// Drive a backend up through `CREATE_DEVICE`, returning the minted logical
    /// device handle.
    fn create_device_one(backend: &mut VulkanBackend) -> proto::Handle {
        let physical_device = enumerate_one(backend);
        let create_resp = backend
            .handle(
                proto::vk_op::CREATE_DEVICE,
                3,
                &create_device_body(physical_device),
            )
            .expect("create device should succeed");
        proto::vk::CreateDeviceResponse::decode(&create_resp)
            .expect("decode create device response")
            .device
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

    #[test]
    fn allocate_memory_after_create_device_returns_memory_handle() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let resp = backend
            .handle(
                proto::vk_op::ALLOCATE_MEMORY,
                5,
                &allocate_memory_body(device, 4096),
            )
            .expect("allocate memory should succeed");

        let decoded = proto::vk::AllocateMemoryResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.memory.kind(), KIND_DEVICE_MEMORY);
        let state = backend
            .memories
            .get(decoded.memory)
            .expect("memory state present");
        assert_eq!(state.device.raw(), device.raw());
        assert_eq!(state.size, 4096);
    }

    #[test]
    fn allocate_memory_with_bogus_device_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_DEVICE, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::ALLOCATE_MEMORY,
                1,
                &allocate_memory_body(bogus, 4096),
            )
            .expect_err("allocate memory with bogus device must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn create_buffer_after_create_device_returns_buffer_handle() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let resp = backend
            .handle(
                proto::vk_op::CREATE_BUFFER,
                6,
                &create_buffer_body(device, 1024, 0x20),
            )
            .expect("create buffer should succeed");

        let decoded = proto::vk::CreateBufferResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.buffer.kind(), KIND_BUFFER);
        let state = backend
            .buffers
            .get(decoded.buffer)
            .expect("buffer state present");
        assert_eq!(state.device.raw(), device.raw());
        assert_eq!(state.size, 1024);
        assert_eq!(state.usage, 0x20);
        assert!(state.bound.is_none());
    }

    #[test]
    fn create_buffer_with_bogus_device_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_DEVICE, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::CREATE_BUFFER,
                1,
                &create_buffer_body(bogus, 1024, 0),
            )
            .expect_err("create buffer with bogus device must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn bind_buffer_memory_succeeds_and_records_binding() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let mem_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_MEMORY,
                5,
                &allocate_memory_body(device, 4096),
            )
            .expect("allocate memory should succeed");
        let memory = proto::vk::AllocateMemoryResponse::decode(&mem_resp)
            .expect("decode memory response")
            .memory;

        let buf_resp = backend
            .handle(
                proto::vk_op::CREATE_BUFFER,
                6,
                &create_buffer_body(device, 1024, 0x20),
            )
            .expect("create buffer should succeed");
        let buffer = proto::vk::CreateBufferResponse::decode(&buf_resp)
            .expect("decode buffer response")
            .buffer;

        let ack = backend
            .handle(
                proto::vk_op::BIND_BUFFER_MEMORY,
                7,
                &bind_buffer_memory_body(buffer, memory, 256),
            )
            .expect("bind buffer memory should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());

        let state = backend.buffers.get(buffer).expect("buffer state present");
        let (bound_memory, offset) = state.bound.expect("binding recorded");
        assert_eq!(bound_memory.raw(), memory.raw());
        assert_eq!(offset, 256);
    }

    #[test]
    fn bind_buffer_memory_with_bogus_buffer_errors() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let mem_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_MEMORY,
                5,
                &allocate_memory_body(device, 4096),
            )
            .expect("allocate memory should succeed");
        let memory = proto::vk::AllocateMemoryResponse::decode(&mem_resp)
            .expect("decode memory response")
            .memory;

        // A buffer handle that was never minted by this backend.
        let bogus_buffer = proto::Handle::new(KIND_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::BIND_BUFFER_MEMORY,
                7,
                &bind_buffer_memory_body(bogus_buffer, memory, 0),
            )
            .expect_err("bind with bogus buffer must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn bind_buffer_memory_with_bogus_memory_errors() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let buf_resp = backend
            .handle(
                proto::vk_op::CREATE_BUFFER,
                6,
                &create_buffer_body(device, 1024, 0),
            )
            .expect("create buffer should succeed");
        let buffer = proto::vk::CreateBufferResponse::decode(&buf_resp)
            .expect("decode buffer response")
            .buffer;

        // A memory handle that was never allocated by this backend.
        let bogus_memory = proto::Handle::new(KIND_DEVICE_MEMORY, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::BIND_BUFFER_MEMORY,
                7,
                &bind_buffer_memory_body(buffer, bogus_memory, 0),
            )
            .expect_err("bind with bogus memory must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
        // The failed bind must not have recorded a binding.
        let state = backend.buffers.get(buffer).expect("buffer state present");
        assert!(state.bound.is_none());
    }

    #[test]
    fn re_bind_buffer_memory_errors() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let mem_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_MEMORY,
                5,
                &allocate_memory_body(device, 4096),
            )
            .expect("allocate memory should succeed");
        let memory = proto::vk::AllocateMemoryResponse::decode(&mem_resp)
            .expect("decode memory response")
            .memory;

        let buf_resp = backend
            .handle(
                proto::vk_op::CREATE_BUFFER,
                6,
                &create_buffer_body(device, 1024, 0),
            )
            .expect("create buffer should succeed");
        let buffer = proto::vk::CreateBufferResponse::decode(&buf_resp)
            .expect("decode buffer response")
            .buffer;

        backend
            .handle(
                proto::vk_op::BIND_BUFFER_MEMORY,
                7,
                &bind_buffer_memory_body(buffer, memory, 0),
            )
            .expect("first bind should succeed");

        let err = backend
            .handle(
                proto::vk_op::BIND_BUFFER_MEMORY,
                8,
                &bind_buffer_memory_body(buffer, memory, 512),
            )
            .expect_err("re-bind must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
        // The original binding must be untouched.
        let state = backend.buffers.get(buffer).expect("buffer state present");
        let (_, offset) = state.bound.expect("original binding intact");
        assert_eq!(offset, 0);
    }

    #[test]
    fn create_command_pool_after_create_device_returns_pool_handle() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);

        let resp = backend
            .handle(
                proto::vk_op::CREATE_COMMAND_POOL,
                7,
                &create_command_pool_body(device, 3),
            )
            .expect("create command pool should succeed");

        let decoded = proto::vk::CreateCommandPoolResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.pool.kind(), KIND_COMMAND_POOL);
        let state = backend
            .command_pools
            .get(decoded.pool)
            .expect("pool state present");
        assert_eq!(state.device.raw(), device.raw());
        assert_eq!(state.queue_family_index, 3);
    }

    #[test]
    fn create_command_pool_with_bogus_device_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_DEVICE, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::CREATE_COMMAND_POOL,
                1,
                &create_command_pool_body(bogus, 0),
            )
            .expect_err("create command pool with bogus device must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn allocate_command_buffer_after_create_pool_returns_buffer_handle() {
        let mut backend = VulkanBackend::new();
        let (_device, pool) = create_command_pool_one(&mut backend);

        let resp = backend
            .handle(
                proto::vk_op::ALLOCATE_COMMAND_BUFFER,
                8,
                &allocate_command_buffer_body(pool),
            )
            .expect("allocate command buffer should succeed");

        let decoded =
            proto::vk::AllocateCommandBufferResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.command_buffer.kind(), KIND_COMMAND_BUFFER);
        let state = backend
            .command_buffers
            .get(decoded.command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.pool.raw(), pool.raw());
    }

    #[test]
    fn allocate_command_buffer_with_bogus_pool_errors() {
        let mut backend = VulkanBackend::new();
        // A handle that was never minted by this backend.
        let bogus = proto::Handle::new(KIND_COMMAND_POOL, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::ALLOCATE_COMMAND_BUFFER,
                1,
                &allocate_command_buffer_body(bogus),
            )
            .expect_err("allocate command buffer with bogus pool must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn queue_submit_validates_queue_and_command_buffer() {
        let mut backend = VulkanBackend::new();
        let (device, queue) = create_queue_one(&mut backend);

        // Create a command pool on the same device and allocate a buffer.
        let pool_resp = backend
            .handle(
                proto::vk_op::CREATE_COMMAND_POOL,
                7,
                &create_command_pool_body(device, 0),
            )
            .expect("create command pool should succeed");
        let pool = proto::vk::CreateCommandPoolResponse::decode(&pool_resp)
            .expect("decode pool response")
            .pool;
        let buf_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_COMMAND_BUFFER,
                8,
                &allocate_command_buffer_body(pool),
            )
            .expect("allocate command buffer should succeed");
        let command_buffer = proto::vk::AllocateCommandBufferResponse::decode(&buf_resp)
            .expect("decode command buffer response")
            .command_buffer;

        let ack = backend
            .handle(
                proto::vk_op::QUEUE_SUBMIT,
                9,
                &queue_submit_body(queue, command_buffer),
            )
            .expect("queue submit should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());
    }

    #[test]
    fn queue_submit_with_bogus_queue_errors() {
        let mut backend = VulkanBackend::new();
        let (_device, pool) = create_command_pool_one(&mut backend);
        let buf_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_COMMAND_BUFFER,
                8,
                &allocate_command_buffer_body(pool),
            )
            .expect("allocate command buffer should succeed");
        let command_buffer = proto::vk::AllocateCommandBufferResponse::decode(&buf_resp)
            .expect("decode command buffer response")
            .command_buffer;

        // A queue handle that was never minted by this backend.
        let bogus_queue = proto::Handle::new(KIND_QUEUE, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::QUEUE_SUBMIT,
                9,
                &queue_submit_body(bogus_queue, command_buffer),
            )
            .expect_err("queue submit with bogus queue must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn queue_submit_with_bogus_command_buffer_errors() {
        let mut backend = VulkanBackend::new();
        let (_device, queue) = create_queue_one(&mut backend);

        // A command buffer handle that was never allocated by this backend.
        let bogus_buffer = proto::Handle::new(KIND_COMMAND_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::QUEUE_SUBMIT,
                9,
                &queue_submit_body(queue, bogus_buffer),
            )
            .expect_err("queue submit with bogus command buffer must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn cmd_copy_buffer_records_and_bumps_count() {
        let mut backend = VulkanBackend::new();
        let (device, command_buffer) = allocate_command_buffer_one(&mut backend);
        let src = create_buffer_one(&mut backend, device);
        let dst = create_buffer_one(&mut backend, device);

        let ack = backend
            .handle(
                proto::vk_op::CMD_COPY_BUFFER,
                10,
                &cmd_copy_buffer_body(command_buffer, src, dst, 512),
            )
            .expect("cmd copy buffer should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());

        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 1);

        // A second record bumps the count again.
        backend
            .handle(
                proto::vk_op::CMD_COPY_BUFFER,
                11,
                &cmd_copy_buffer_body(command_buffer, src, dst, 256),
            )
            .expect("second cmd copy buffer should succeed");
        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 2);
    }

    #[test]
    fn cmd_copy_buffer_with_bogus_command_buffer_errors() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);
        let src = create_buffer_one(&mut backend, device);
        let dst = create_buffer_one(&mut backend, device);

        // A command buffer handle that was never allocated by this backend.
        let bogus = proto::Handle::new(KIND_COMMAND_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::CMD_COPY_BUFFER,
                10,
                &cmd_copy_buffer_body(bogus, src, dst, 512),
            )
            .expect_err("cmd copy buffer with bogus command buffer must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn cmd_copy_buffer_with_bogus_src_errors() {
        let mut backend = VulkanBackend::new();
        let (device, command_buffer) = allocate_command_buffer_one(&mut backend);
        let dst = create_buffer_one(&mut backend, device);

        // A source buffer handle that was never minted by this backend.
        let bogus_src = proto::Handle::new(KIND_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::CMD_COPY_BUFFER,
                10,
                &cmd_copy_buffer_body(command_buffer, bogus_src, dst, 512),
            )
            .expect_err("cmd copy buffer with bogus src must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
        // The failed record must not have bumped the count.
        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 0);
    }

    #[test]
    fn cmd_copy_buffer_with_bogus_dst_errors() {
        let mut backend = VulkanBackend::new();
        let (device, command_buffer) = allocate_command_buffer_one(&mut backend);
        let src = create_buffer_one(&mut backend, device);

        // A destination buffer handle that was never minted by this backend.
        let bogus_dst = proto::Handle::new(KIND_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::CMD_COPY_BUFFER,
                10,
                &cmd_copy_buffer_body(command_buffer, src, bogus_dst, 512),
            )
            .expect_err("cmd copy buffer with bogus dst must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
        // The failed record must not have bumped the count.
        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 0);
    }

    #[test]
    fn cmd_draw_records_and_bumps_count() {
        let mut backend = VulkanBackend::new();
        let (_device, command_buffer) = allocate_command_buffer_one(&mut backend);

        let ack = backend
            .handle(
                proto::vk_op::CMD_DRAW,
                12,
                &cmd_draw_body(command_buffer, 3, 1),
            )
            .expect("cmd draw should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());

        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 1);

        // A second record bumps the count again.
        backend
            .handle(
                proto::vk_op::CMD_DRAW,
                13,
                &cmd_draw_body(command_buffer, 6, 2),
            )
            .expect("second cmd draw should succeed");
        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 2);
    }

    #[test]
    fn cmd_draw_then_copy_share_recorded_count() {
        let mut backend = VulkanBackend::new();
        let (device, command_buffer) = allocate_command_buffer_one(&mut backend);
        let src = create_buffer_one(&mut backend, device);
        let dst = create_buffer_one(&mut backend, device);

        backend
            .handle(
                proto::vk_op::CMD_DRAW,
                12,
                &cmd_draw_body(command_buffer, 3, 1),
            )
            .expect("cmd draw should succeed");
        backend
            .handle(
                proto::vk_op::CMD_COPY_BUFFER,
                13,
                &cmd_copy_buffer_body(command_buffer, src, dst, 512),
            )
            .expect("cmd copy buffer should succeed");

        let state = backend
            .command_buffers
            .get(command_buffer)
            .expect("command buffer state present");
        assert_eq!(state.recorded, 2);
    }

    #[test]
    fn cmd_draw_with_bogus_command_buffer_errors() {
        let mut backend = VulkanBackend::new();
        // A command buffer handle that was never allocated by this backend.
        let bogus = proto::Handle::new(KIND_COMMAND_BUFFER, 0, 999);
        let err = backend
            .handle(proto::vk_op::CMD_DRAW, 12, &cmd_draw_body(bogus, 3, 1))
            .expect_err("cmd draw with bogus command buffer must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_buffer_removes_and_acks() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);
        let buffer = create_buffer_one(&mut backend, device);

        let ack = backend
            .handle(
                proto::vk_op::DESTROY_BUFFER,
                14,
                &destroy_buffer_body(buffer),
            )
            .expect("destroy buffer should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());
        // The buffer no longer resolves in its table.
        assert!(backend.buffers.get(buffer).is_none());
    }

    #[test]
    fn destroy_buffer_with_bogus_handle_errors() {
        let mut backend = VulkanBackend::new();
        // A buffer handle that was never minted by this backend.
        let bogus = proto::Handle::new(KIND_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::DESTROY_BUFFER,
                14,
                &destroy_buffer_body(bogus),
            )
            .expect_err("destroy buffer with bogus handle must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_buffer_twice_errors_on_second() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);
        let buffer = create_buffer_one(&mut backend, device);

        backend
            .handle(
                proto::vk_op::DESTROY_BUFFER,
                14,
                &destroy_buffer_body(buffer),
            )
            .expect("first destroy should succeed");
        // A second destroy of the same handle must error: it is already gone.
        let err = backend
            .handle(
                proto::vk_op::DESTROY_BUFFER,
                15,
                &destroy_buffer_body(buffer),
            )
            .expect_err("second destroy must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn free_memory_removes_and_acks() {
        let mut backend = VulkanBackend::new();
        let device = create_device_one(&mut backend);
        let mem_resp = backend
            .handle(
                proto::vk_op::ALLOCATE_MEMORY,
                5,
                &allocate_memory_body(device, 4096),
            )
            .expect("allocate memory should succeed");
        let memory = proto::vk::AllocateMemoryResponse::decode(&mem_resp)
            .expect("decode memory response")
            .memory;

        let ack = backend
            .handle(proto::vk_op::FREE_MEMORY, 16, &free_memory_body(memory))
            .expect("free memory should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());
        // The memory no longer resolves in its table.
        assert!(backend.memories.get(memory).is_none());
    }

    #[test]
    fn free_memory_with_bogus_handle_errors() {
        let mut backend = VulkanBackend::new();
        // A memory handle that was never allocated by this backend.
        let bogus = proto::Handle::new(KIND_DEVICE_MEMORY, 0, 999);
        let err = backend
            .handle(proto::vk_op::FREE_MEMORY, 16, &free_memory_body(bogus))
            .expect_err("free memory with bogus handle must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_command_pool_removes_and_acks() {
        let mut backend = VulkanBackend::new();
        let (_device, pool) = create_command_pool_one(&mut backend);

        let ack = backend
            .handle(
                proto::vk_op::DESTROY_COMMAND_POOL,
                17,
                &destroy_command_pool_body(pool),
            )
            .expect("destroy command pool should succeed");
        // The reply is an empty ack body.
        assert!(ack.is_empty());
        // The pool no longer resolves in its table.
        assert!(backend.command_pools.get(pool).is_none());
    }

    #[test]
    fn destroy_command_pool_with_bogus_handle_errors() {
        let mut backend = VulkanBackend::new();
        // A command pool handle that was never minted by this backend.
        let bogus = proto::Handle::new(KIND_COMMAND_POOL, 0, 999);
        let err = backend
            .handle(
                proto::vk_op::DESTROY_COMMAND_POOL,
                17,
                &destroy_command_pool_body(bogus),
            )
            .expect_err("destroy command pool with bogus handle must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }
}
