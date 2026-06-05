//! Vulkan client shim (Rust-level).
//!
//! Serializes Vulkan entrypoints into [`graftx_protocol`] frames and forwards
//! them over a [`graftx_transport::Transport`] to the server's Vulkan backend.
//! These are plain Rust functions; C-ABI export of the `vk*` entry points comes
//! later. This is a pure-Rust shim — no `ash`, no Vulkan SDK, no GPU.
use graftx_protocol as proto;
use graftx_transport::Transport;

use crate::ClientError;

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

/// Issue `vkCreateInstance` and return the new instance [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_INSTANCE`](proto::vk_op::CREATE_INSTANCE) request carrying a
/// [`CreateInstanceRequest`](proto::vk::CreateInstanceRequest), awaits the
/// correlated response, validates its opcode and kind, decodes the
/// [`CreateInstanceResponse`](proto::vk::CreateInstanceResponse), and verifies
/// the returned handle names a `VkInstance`.
pub fn create_instance<T: Transport>(
    t: &mut T,
    app_api_version: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::CreateInstanceRequest { app_api_version }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::CREATE_INSTANCE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::CREATE_INSTANCE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::CreateInstanceResponse::decode(b)?;
    if resp.instance.kind() != KIND_INSTANCE {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.instance.kind() as u16,
        )));
    }
    Ok(resp.instance)
}

/// Issue `vkEnumeratePhysicalDevices` and return the physical-device handles.
///
/// Sends an [`ENUMERATE_PHYSICAL_DEVICES`](proto::vk_op::ENUMERATE_PHYSICAL_DEVICES)
/// request carrying an
/// [`EnumeratePhysicalDevicesRequest`](proto::vk::EnumeratePhysicalDevicesRequest)
/// naming `instance`, awaits the correlated response, validates its opcode and
/// kind, decodes the
/// [`EnumeratePhysicalDevicesResponse`](proto::vk::EnumeratePhysicalDevicesResponse),
/// and verifies every returned handle names a `VkPhysicalDevice`.
pub fn enumerate_physical_devices<T: Transport>(
    t: &mut T,
    instance: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<Vec<proto::Handle>, ClientError> {
    let mut body = Vec::new();
    proto::vk::EnumeratePhysicalDevicesRequest { instance }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::ENUMERATE_PHYSICAL_DEVICES,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::ENUMERATE_PHYSICAL_DEVICES || h.kind != proto::FrameKind::Response
    {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::EnumeratePhysicalDevicesResponse::decode(b)?;
    for device in &resp.devices {
        if device.kind() != KIND_PHYSICAL_DEVICE {
            return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
                device.kind() as u16,
            )));
        }
    }
    Ok(resp.devices)
}

/// Issue `vkCreateDevice` and return the new logical-device [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_DEVICE`](proto::vk_op::CREATE_DEVICE) request carrying a
/// [`CreateDeviceRequest`](proto::vk::CreateDeviceRequest) naming
/// `physical_device`, awaits the correlated response, validates its opcode and
/// kind, decodes the [`CreateDeviceResponse`](proto::vk::CreateDeviceResponse),
/// and verifies the returned handle names a `VkDevice`.
pub fn create_device<T: Transport>(
    t: &mut T,
    physical_device: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::CreateDeviceRequest { physical_device }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::CREATE_DEVICE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::CREATE_DEVICE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::CreateDeviceResponse::decode(b)?;
    if resp.device.kind() != KIND_DEVICE {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.device.kind() as u16,
        )));
    }
    Ok(resp.device)
}

/// Issue `vkGetDeviceQueue` and return the queue [`Handle`](proto::Handle).
///
/// Sends a [`GET_DEVICE_QUEUE`](proto::vk_op::GET_DEVICE_QUEUE) request carrying
/// a [`GetDeviceQueueRequest`](proto::vk::GetDeviceQueueRequest) naming `device`
/// and the queue family and queue indices, awaits the correlated response,
/// validates its opcode and kind, decodes the
/// [`GetDeviceQueueResponse`](proto::vk::GetDeviceQueueResponse), and verifies
/// the returned handle names a `VkQueue`.
pub fn get_device_queue<T: Transport>(
    t: &mut T,
    device: proto::Handle,
    queue_family_index: u32,
    queue_index: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::GetDeviceQueueRequest {
        device,
        queue_family_index,
        queue_index,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::GET_DEVICE_QUEUE,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::GET_DEVICE_QUEUE || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::GetDeviceQueueResponse::decode(b)?;
    if resp.queue.kind() != KIND_QUEUE {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.queue.kind() as u16,
        )));
    }
    Ok(resp.queue)
}

/// Issue `vkAllocateMemory` and return the new device-memory [`Handle`](proto::Handle).
///
/// Sends an [`ALLOCATE_MEMORY`](proto::vk_op::ALLOCATE_MEMORY) request carrying an
/// [`AllocateMemoryRequest`](proto::vk::AllocateMemoryRequest) naming `device` and
/// the allocation `size`, awaits the correlated response, validates its opcode and
/// kind, decodes the [`AllocateMemoryResponse`](proto::vk::AllocateMemoryResponse),
/// and verifies the returned handle names a `VkDeviceMemory`.
pub fn allocate_memory<T: Transport>(
    t: &mut T,
    device: proto::Handle,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::AllocateMemoryRequest { device, size }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::ALLOCATE_MEMORY,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::ALLOCATE_MEMORY || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::AllocateMemoryResponse::decode(b)?;
    if resp.memory.kind() != KIND_DEVICE_MEMORY {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.memory.kind() as u16,
        )));
    }
    Ok(resp.memory)
}

/// Issue `vkCreateBuffer` and return the new buffer [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_BUFFER`](proto::vk_op::CREATE_BUFFER) request carrying a
/// [`CreateBufferRequest`](proto::vk::CreateBufferRequest) naming `device`, the
/// buffer `size`, and `usage` flags, awaits the correlated response, validates its
/// opcode and kind, decodes the
/// [`CreateBufferResponse`](proto::vk::CreateBufferResponse), and verifies the
/// returned handle names a `VkBuffer`.
pub fn create_buffer<T: Transport>(
    t: &mut T,
    device: proto::Handle,
    size: u64,
    usage: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::CreateBufferRequest {
        device,
        size,
        usage,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::CREATE_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::CREATE_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::CreateBufferResponse::decode(b)?;
    if resp.buffer.kind() != KIND_BUFFER {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.buffer.kind() as u16,
        )));
    }
    Ok(resp.buffer)
}

/// Issue `vkBindBufferMemory`, binding `memory` to `buffer` at `offset`.
///
/// Sends a [`BIND_BUFFER_MEMORY`](proto::vk_op::BIND_BUFFER_MEMORY) request
/// carrying a [`BindBufferMemoryRequest`](proto::vk::BindBufferMemoryRequest),
/// awaits the correlated response, and validates its opcode and kind. The reply
/// body is an empty acknowledgement, so nothing is decoded.
pub fn bind_buffer_memory<T: Transport>(
    t: &mut T,
    buffer: proto::Handle,
    memory: proto::Handle,
    offset: u64,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::BindBufferMemoryRequest {
        buffer,
        memory,
        offset,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::BIND_BUFFER_MEMORY,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::BIND_BUFFER_MEMORY || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue `vkCreateCommandPool` and return the new command-pool [`Handle`](proto::Handle).
///
/// Sends a [`CREATE_COMMAND_POOL`](proto::vk_op::CREATE_COMMAND_POOL) request
/// carrying a [`CreateCommandPoolRequest`](proto::vk::CreateCommandPoolRequest)
/// naming `device` and the `queue_family_index` the pool's buffers are submitted
/// to, awaits the correlated response, validates its opcode and kind, decodes the
/// [`CreateCommandPoolResponse`](proto::vk::CreateCommandPoolResponse), and
/// verifies the returned handle names a `VkCommandPool`.
pub fn create_command_pool<T: Transport>(
    t: &mut T,
    device: proto::Handle,
    queue_family_index: u32,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::CreateCommandPoolRequest {
        device,
        queue_family_index,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::CREATE_COMMAND_POOL,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::CREATE_COMMAND_POOL || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::CreateCommandPoolResponse::decode(b)?;
    if resp.pool.kind() != KIND_COMMAND_POOL {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.pool.kind() as u16,
        )));
    }
    Ok(resp.pool)
}

/// Issue `vkAllocateCommandBuffers` and return the new command-buffer [`Handle`](proto::Handle).
///
/// Sends an [`ALLOCATE_COMMAND_BUFFER`](proto::vk_op::ALLOCATE_COMMAND_BUFFER)
/// request carrying an
/// [`AllocateCommandBufferRequest`](proto::vk::AllocateCommandBufferRequest)
/// naming the `pool` the buffer is allocated from, awaits the correlated
/// response, validates its opcode and kind, decodes the
/// [`AllocateCommandBufferResponse`](proto::vk::AllocateCommandBufferResponse),
/// and verifies the returned handle names a `VkCommandBuffer`.
pub fn allocate_command_buffer<T: Transport>(
    t: &mut T,
    pool: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<proto::Handle, ClientError> {
    let mut body = Vec::new();
    proto::vk::AllocateCommandBufferRequest { pool }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::ALLOCATE_COMMAND_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::ALLOCATE_COMMAND_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    let resp = proto::vk::AllocateCommandBufferResponse::decode(b)?;
    if resp.command_buffer.kind() != KIND_COMMAND_BUFFER {
        return Err(ClientError::Protocol(proto::ProtocolError::BadKind(
            resp.command_buffer.kind() as u16,
        )));
    }
    Ok(resp.command_buffer)
}

/// Issue `vkQueueSubmit`, submitting `command_buffer` to `queue`.
///
/// Sends a [`QUEUE_SUBMIT`](proto::vk_op::QUEUE_SUBMIT) request carrying a
/// [`QueueSubmitRequest`](proto::vk::QueueSubmitRequest), awaits the correlated
/// response, and validates its opcode and kind. The reply body is an empty
/// acknowledgement, so nothing is decoded.
pub fn queue_submit<T: Transport>(
    t: &mut T,
    queue: proto::Handle,
    command_buffer: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::QueueSubmitRequest {
        queue,
        command_buffer,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::QUEUE_SUBMIT,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::QUEUE_SUBMIT || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue `vkCmdCopyBuffer`, recording a `src` → `dst` copy of `size` bytes into
/// `command_buffer`.
///
/// Sends a [`CMD_COPY_BUFFER`](proto::vk_op::CMD_COPY_BUFFER) request carrying a
/// [`CmdCopyBufferRequest`](proto::vk::CmdCopyBufferRequest), awaits the
/// correlated response, and validates its opcode and kind. The reply body is an
/// empty acknowledgement, so nothing is decoded.
pub fn cmd_copy_buffer<T: Transport>(
    t: &mut T,
    command_buffer: proto::Handle,
    src: proto::Handle,
    dst: proto::Handle,
    size: u64,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::CmdCopyBufferRequest {
        command_buffer,
        src,
        dst,
        size,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::CMD_COPY_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::CMD_COPY_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue `vkCmdDraw`, recording a non-indexed draw of `vertex_count` vertices
/// across `instance_count` instances into `command_buffer`.
///
/// Sends a [`CMD_DRAW`](proto::vk_op::CMD_DRAW) request carrying a
/// [`CmdDrawRequest`](proto::vk::CmdDrawRequest), awaits the correlated
/// response, and validates its opcode and kind. The reply body is an empty
/// acknowledgement, so nothing is decoded.
pub fn cmd_draw<T: Transport>(
    t: &mut T,
    command_buffer: proto::Handle,
    vertex_count: u32,
    instance_count: u32,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::CmdDrawRequest {
        command_buffer,
        vertex_count,
        instance_count,
    }
    .encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::CMD_DRAW,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::CMD_DRAW || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue `vkDestroyBuffer`, destroying `buffer`.
///
/// Sends a [`DESTROY_BUFFER`](proto::vk_op::DESTROY_BUFFER) request carrying a
/// [`DestroyBufferRequest`](proto::vk::DestroyBufferRequest), awaits the
/// correlated response, and validates its opcode and kind. The reply body is an
/// empty acknowledgement, so nothing is decoded.
pub fn destroy_buffer<T: Transport>(
    t: &mut T,
    buffer: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::DestroyBufferRequest { buffer }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::DESTROY_BUFFER,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::DESTROY_BUFFER || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue `vkFreeMemory`, freeing `memory`.
///
/// Sends a [`FREE_MEMORY`](proto::vk_op::FREE_MEMORY) request carrying a
/// [`FreeMemoryRequest`](proto::vk::FreeMemoryRequest), awaits the correlated
/// response, and validates its opcode and kind. The reply body is an empty
/// acknowledgement, so nothing is decoded.
pub fn free_memory<T: Transport>(
    t: &mut T,
    memory: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::FreeMemoryRequest { memory }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::FREE_MEMORY,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::FREE_MEMORY || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}

/// Issue `vkDestroyCommandPool`, destroying `pool`.
///
/// Sends a [`DESTROY_COMMAND_POOL`](proto::vk_op::DESTROY_COMMAND_POOL) request
/// carrying a [`DestroyCommandPoolRequest`](proto::vk::DestroyCommandPoolRequest),
/// awaits the correlated response, and validates its opcode and kind. The reply
/// body is an empty acknowledgement, so nothing is decoded.
pub fn destroy_command_pool<T: Transport>(
    t: &mut T,
    pool: proto::Handle,
    req_id: u32,
    seq: u64,
) -> Result<(), ClientError> {
    let mut body = Vec::new();
    proto::vk::DestroyCommandPoolRequest { pool }.encode(&mut body);
    let header = proto::FrameHeader {
        version: proto::PROTOCOL_MAJOR,
        flags: 0,
        kind: proto::FrameKind::Request,
        opcode: proto::vk_op::DESTROY_COMMAND_POOL,
        req_id,
        seq,
        body_len: body.len() as u32,
    };
    t.send(&proto::encode_frame(&header, &body))?;

    let reply = t.recv()?;
    let (h, _b) = proto::decode_frame(&reply)?;
    if h.opcode != proto::vk_op::DESTROY_COMMAND_POOL || h.kind != proto::FrameKind::Response {
        return Err(ClientError::UnexpectedReply {
            opcode: h.opcode,
            kind: h.kind,
        });
    }
    Ok(())
}
