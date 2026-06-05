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
