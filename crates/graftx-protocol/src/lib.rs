//! GraftX wire protocol.
//!
//! Defines the control-plane framing, opcode scheme, and handshake messages
//! shared by the Linux client shim and the Windows server. This crate is the
//! single source of truth for the wire contract; both ends link it.
//!
//! Layering (see the Transport chapter): the transport frame carries a magic +
//! channel + length and wraps an *opaque* body. That body is the protocol frame
//! defined here — a [`FrameHeader`] followed by a per-opcode payload. The magic
//! lives in the transport layer, not in this header.
#![forbid(unsafe_op_in_unsafe_fn)]

/// Protocol major version. Bumped on a breaking wire change; while `0` the
/// format is pre-stable and may change freely.
pub const PROTOCOL_MAJOR: u16 = 0;
/// Protocol minor version. Bumped on backward-compatible additions.
pub const PROTOCOL_MINOR: u16 = 0;

/// Default negotiated cap on a single control-frame body, in bytes (1 MiB).
/// The handshake may lower this; payloads larger than the negotiated cap move
/// over the bulk plane instead.
pub const DEFAULT_MAX_FRAME_BODY: u32 = 1 << 20;

/// Errors raised while encoding or decoding the wire format.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The buffer ended before a full structure could be decoded.
    #[error("unexpected end of buffer")]
    UnexpectedEof,
    /// The decoded opcode does not map to any known entrypoint.
    #[error("unknown opcode: {0:#010x}")]
    UnknownOpcode(u32),
    /// The decoded frame kind is not one of the known kinds.
    #[error("unsupported frame kind: {0}")]
    BadKind(u16),
    /// The peer speaks an incompatible protocol major version.
    #[error("unsupported protocol version: {major}.{minor}")]
    BadVersion {
        /// Major version the peer offered.
        major: u16,
        /// Minor version the peer offered.
        minor: u16,
    },
    /// A frame body exceeded the negotiated maximum.
    #[error("frame body too large: {0} bytes")]
    FrameTooLarge(u32),
    /// The declared body length did not match the bytes present.
    #[error("body length mismatch: header says {declared}, found {actual}")]
    BodyLenMismatch {
        /// Length declared in the header.
        declared: u32,
        /// Bytes actually present after the header.
        actual: usize,
    },
}

/// API namespace occupying the high byte of an [`opcode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ApiId {
    /// Core protocol entrypoints (handshake, no-op, control).
    Core = 0x00,
    /// Vulkan.
    Vulkan = 0x01,
    /// OpenGL / OpenGL ES / EGL / GLX.
    OpenGl = 0x02,
    /// CUDA.
    Cuda = 0x03,
    /// OpenCL.
    OpenCl = 0x04,
    /// ROCm / HIP.
    Hip = 0x05,
    /// Intel Level Zero.
    LevelZero = 0x06,
    /// Video codecs (VA-API / VDPAU / NVENC / NVDEC / Vulkan Video).
    Video = 0x07,
    /// WebGPU.
    WebGpu = 0x08,
    /// NVIDIA OptiX ray tracing.
    OptiX = 0x09,
    /// SYCL.
    Sycl = 0x0A,
    /// AMD Advanced Media Framework (AMF) video encode.
    Amf = 0x0B,
}

/// Build an opcode from its API namespace and 24-bit call id.
///
/// An opcode is a `u32` whose high byte is the [`ApiId`] and whose low 24 bits
/// are the call id within that API (e.g. Vulkan opcodes are `0x01_xxxxxx`).
pub const fn opcode(api: ApiId, call: u32) -> u32 {
    ((api as u32) << 24) | (call & 0x00FF_FFFF)
}

/// The API-namespace byte of an opcode.
pub const fn opcode_api(op: u32) -> u8 {
    (op >> 24) as u8
}

/// The 24-bit call id of an opcode.
pub const fn opcode_call(op: u32) -> u32 {
    op & 0x00FF_FFFF
}

/// Core protocol opcodes (under [`ApiId::Core`]).
pub mod core_op {
    use super::{opcode, ApiId};

    /// Client → server: open a session and offer protocol parameters.
    pub const HELLO: u32 = opcode(ApiId::Core, 0x000001);
    /// Server → client: accept the session and confirm parameters.
    pub const WELCOME: u32 = opcode(ApiId::Core, 0x000002);
    /// A no-op round-trip used to validate the pipe end to end.
    pub const NOOP: u32 = opcode(ApiId::Core, 0x000003);
}

/// Vulkan opcodes (under [`ApiId::Vulkan`]).
pub mod vk_op {
    use super::{opcode, ApiId};

    /// `vkCreateInstance`: create a Vulkan instance.
    pub const CREATE_INSTANCE: u32 = opcode(ApiId::Vulkan, 0x0001);
    /// `vkDestroyInstance`: destroy a Vulkan instance.
    pub const DESTROY_INSTANCE: u32 = opcode(ApiId::Vulkan, 0x0002);
    /// `vkEnumeratePhysicalDevices`: list the physical devices on an instance.
    pub const ENUMERATE_PHYSICAL_DEVICES: u32 = opcode(ApiId::Vulkan, 0x0003);
    /// `vkGetPhysicalDeviceProperties`: query a physical device's properties.
    pub const GET_PHYSICAL_DEVICE_PROPERTIES: u32 = opcode(ApiId::Vulkan, 0x0004);
    /// `vkCreateDevice`: create a logical device from a physical device.
    pub const CREATE_DEVICE: u32 = opcode(ApiId::Vulkan, 0x0005);
    /// `vkDestroyDevice`: destroy a logical device.
    pub const DESTROY_DEVICE: u32 = opcode(ApiId::Vulkan, 0x0006);
    /// `vkGetDeviceQueue`: retrieve a queue handle from a logical device.
    pub const GET_DEVICE_QUEUE: u32 = opcode(ApiId::Vulkan, 0x0007);
    /// `vkDeviceWaitIdle`: block until a logical device is idle.
    pub const DEVICE_WAIT_IDLE: u32 = opcode(ApiId::Vulkan, 0x0008);
    /// `vkAllocateMemory`: allocate a block of device memory.
    pub const ALLOCATE_MEMORY: u32 = opcode(ApiId::Vulkan, 0x0010);
    /// `vkCreateBuffer`: create a buffer object on a device.
    pub const CREATE_BUFFER: u32 = opcode(ApiId::Vulkan, 0x0011);
    /// `vkBindBufferMemory`: bind device memory to a buffer.
    pub const BIND_BUFFER_MEMORY: u32 = opcode(ApiId::Vulkan, 0x0012);
    /// `vkCreateCommandPool`: create a command pool on a device.
    pub const CREATE_COMMAND_POOL: u32 = opcode(ApiId::Vulkan, 0x0020);
    /// `vkAllocateCommandBuffers`: allocate a command buffer from a pool.
    pub const ALLOCATE_COMMAND_BUFFER: u32 = opcode(ApiId::Vulkan, 0x0021);
    /// `vkQueueSubmit`: submit a command buffer to a queue.
    pub const QUEUE_SUBMIT: u32 = opcode(ApiId::Vulkan, 0x0022);
    /// `vkCmdCopyBuffer`: record a buffer-to-buffer copy into a command buffer.
    pub const CMD_COPY_BUFFER: u32 = opcode(ApiId::Vulkan, 0x0023);
    /// `vkCmdDraw`: record a non-indexed draw into a command buffer.
    pub const CMD_DRAW: u32 = opcode(ApiId::Vulkan, 0x0024);
}

/// OpenGL opcodes (under [`ApiId::OpenGl`]).
pub mod gl_op {
    use super::{opcode, ApiId};

    /// Create a rendering context.
    pub const CREATE_CONTEXT: u32 = opcode(ApiId::OpenGl, 0x0001);
    /// Make a context current on the calling session.
    pub const MAKE_CURRENT: u32 = opcode(ApiId::OpenGl, 0x0002);
    /// Generate a buffer object within a context.
    pub const GEN_BUFFER: u32 = opcode(ApiId::OpenGl, 0x0003);
}

/// CUDA opcodes (under [`ApiId::Cuda`]).
pub mod cuda_op {
    use super::{opcode, ApiId};

    /// `cuCtxCreate`: create a CUDA context on a device.
    pub const CTX_CREATE: u32 = opcode(ApiId::Cuda, 0x0001);
    /// `cuMemAlloc`: allocate a block of device memory in a context.
    pub const MEM_ALLOC: u32 = opcode(ApiId::Cuda, 0x0002);
    /// `cuMemFree`: free a previously allocated device pointer.
    pub const MEM_FREE: u32 = opcode(ApiId::Cuda, 0x0003);
}

/// HIP opcodes (under [`ApiId::Hip`]).
pub mod hip_op {
    use super::{opcode, ApiId};

    /// `hipMalloc`: allocate a block of device memory.
    pub const MALLOC: u32 = opcode(ApiId::Hip, 0x0001);
    /// `hipFree`: free a previously allocated device pointer.
    pub const FREE: u32 = opcode(ApiId::Hip, 0x0002);
    /// `hipStreamCreate`: create an asynchronous stream.
    pub const STREAM_CREATE: u32 = opcode(ApiId::Hip, 0x0003);
}

/// OpenCL opcodes (under [`ApiId::OpenCl`]).
pub mod cl_op {
    use super::{opcode, ApiId};

    /// `clCreateContext`: create an OpenCL context.
    pub const CREATE_CONTEXT: u32 = opcode(ApiId::OpenCl, 0x0001);
    /// `clCreateBuffer`: create a memory buffer in a context.
    pub const CREATE_BUFFER: u32 = opcode(ApiId::OpenCl, 0x0002);
    /// `clReleaseMemObject`: release a previously created memory buffer.
    pub const RELEASE_BUFFER: u32 = opcode(ApiId::OpenCl, 0x0003);
}

/// Level Zero opcodes (under [`ApiId::LevelZero`]).
pub mod l0_op {
    use super::{opcode, ApiId};

    /// `zeContextCreate`: create a Level Zero context.
    pub const CONTEXT_CREATE: u32 = opcode(ApiId::LevelZero, 0x0001);
    /// `zeMemAllocDevice`: allocate a block of device memory in a context.
    pub const MEM_ALLOC_DEVICE: u32 = opcode(ApiId::LevelZero, 0x0002);
    /// `zeMemFree`: free a previously allocated device pointer.
    pub const MEM_FREE: u32 = opcode(ApiId::LevelZero, 0x0003);
}

/// Video codec opcodes (under [`ApiId::Video`]).
pub mod video_op {
    use super::{opcode, ApiId};

    /// Create a decode session for a given codec and frame geometry.
    pub const CREATE_DECODE_SESSION: u32 = opcode(ApiId::Video, 0x0001);
    /// Submit a coded bitstream to a session and decode one frame.
    pub const DECODE_FRAME: u32 = opcode(ApiId::Video, 0x0002);
    /// Destroy a previously created decode session.
    pub const DESTROY_SESSION: u32 = opcode(ApiId::Video, 0x0003);
}

/// WebGPU opcodes (under [`ApiId::WebGpu`]).
pub mod wgpu_op {
    use super::{opcode, ApiId};

    /// `requestDevice`: acquire a logical device from an adapter.
    pub const REQUEST_DEVICE: u32 = opcode(ApiId::WebGpu, 0x0001);
    /// `createBuffer`: create a buffer on a device.
    pub const CREATE_BUFFER: u32 = opcode(ApiId::WebGpu, 0x0002);
    /// `destroy`: destroy a previously created buffer.
    pub const DESTROY_BUFFER: u32 = opcode(ApiId::WebGpu, 0x0003);
}

/// OptiX opcodes (under [`ApiId::OptiX`]).
pub mod optix_op {
    use super::{opcode, ApiId};

    /// `optixDeviceContextCreate`: create an OptiX device context.
    pub const CONTEXT_CREATE: u32 = opcode(ApiId::OptiX, 0x0001);
    /// `optixPipelineCreate`: create a ray-tracing pipeline in a context.
    pub const PIPELINE_CREATE: u32 = opcode(ApiId::OptiX, 0x0002);
    /// Destroy a previously created OptiX context or pipeline handle.
    pub const DESTROY: u32 = opcode(ApiId::OptiX, 0x0003);
}

/// SYCL opcodes (under [`ApiId::Sycl`]).
pub mod sycl_op {
    use super::{opcode, ApiId};

    /// Create a SYCL queue.
    pub const QUEUE_CREATE: u32 = opcode(ApiId::Sycl, 0x0001);
    /// `sycl::malloc_device`: allocate a block of device memory on a queue.
    pub const MALLOC_DEVICE: u32 = opcode(ApiId::Sycl, 0x0002);
    /// `sycl::free`: free a previously allocated device pointer.
    pub const FREE: u32 = opcode(ApiId::Sycl, 0x0003);
}

/// AMF opcodes (under [`ApiId::Amf`]).
pub mod amf_op {
    use super::{opcode, ApiId};

    /// Create an AMF encoder for a given codec and frame geometry.
    pub const CREATE_ENCODER: u32 = opcode(ApiId::Amf, 0x0001);
    /// Submit one raw frame to an encoder and produce a coded packet.
    pub const ENCODE_FRAME: u32 = opcode(ApiId::Amf, 0x0002);
    /// Destroy a previously created encoder.
    pub const DESTROY_ENCODER: u32 = opcode(ApiId::Amf, 0x0003);
}

/// Vulkan request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the Vulkan opcodes in [`vk_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
pub mod vk {
    use super::{Handle, ProtocolError, Reader};

    /// Request body for [`vk_op::CREATE_INSTANCE`](super::vk_op::CREATE_INSTANCE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateInstanceRequest {
        /// Application-requested Vulkan API version.
        pub app_api_version: u32,
    }

    impl CreateInstanceRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.app_api_version.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                app_api_version: r.u32()?,
            })
        }
    }

    /// Response body for [`vk_op::CREATE_INSTANCE`](super::vk_op::CREATE_INSTANCE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateInstanceResponse {
        /// Handle naming the newly created instance.
        pub instance: Handle,
    }

    impl CreateInstanceResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.instance.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                instance: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`vk_op::ENUMERATE_PHYSICAL_DEVICES`](super::vk_op::ENUMERATE_PHYSICAL_DEVICES).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EnumeratePhysicalDevicesRequest {
        /// Handle naming the instance whose devices are listed.
        pub instance: Handle,
    }

    impl EnumeratePhysicalDevicesRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.instance.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                instance: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Response body for
    /// [`vk_op::ENUMERATE_PHYSICAL_DEVICES`](super::vk_op::ENUMERATE_PHYSICAL_DEVICES).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EnumeratePhysicalDevicesResponse {
        /// Handles naming the physical devices on the instance.
        pub devices: Vec<Handle>,
    }

    impl EnumeratePhysicalDevicesResponse {
        /// Append the encoded body to `out`: a `u32` count followed by that many
        /// raw `u64` device handles.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&(self.devices.len() as u32).to_le_bytes());
            for device in &self.devices {
                out.extend_from_slice(&device.raw().to_le_bytes());
            }
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            let count = r.u32()?;
            let mut devices = Vec::with_capacity(count as usize);
            for _ in 0..count {
                devices.push(Handle::from_raw(r.u64()?));
            }
            Ok(Self { devices })
        }
    }

    /// Request body for [`vk_op::CREATE_DEVICE`](super::vk_op::CREATE_DEVICE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateDeviceRequest {
        /// Handle naming the physical device the logical device is created from.
        pub physical_device: Handle,
    }

    impl CreateDeviceRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.physical_device.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                physical_device: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Response body for [`vk_op::CREATE_DEVICE`](super::vk_op::CREATE_DEVICE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateDeviceResponse {
        /// Handle naming the newly created logical device.
        pub device: Handle,
    }

    impl CreateDeviceResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`vk_op::GET_DEVICE_QUEUE`](super::vk_op::GET_DEVICE_QUEUE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GetDeviceQueueRequest {
        /// Handle naming the logical device the queue belongs to.
        pub device: Handle,
        /// Index of the queue family.
        pub queue_family_index: u32,
        /// Index of the queue within its family.
        pub queue_index: u32,
    }

    impl GetDeviceQueueRequest {
        /// Append the encoded body to `out`: a raw `u64` device handle followed
        /// by the `u32` queue family index and `u32` queue index.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
            out.extend_from_slice(&self.queue_family_index.to_le_bytes());
            out.extend_from_slice(&self.queue_index.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
                queue_family_index: r.u32()?,
                queue_index: r.u32()?,
            })
        }
    }

    /// Response body for [`vk_op::GET_DEVICE_QUEUE`](super::vk_op::GET_DEVICE_QUEUE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GetDeviceQueueResponse {
        /// Handle naming the retrieved queue.
        pub queue: Handle,
    }

    impl GetDeviceQueueResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.queue.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                queue: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`vk_op::ALLOCATE_MEMORY`](super::vk_op::ALLOCATE_MEMORY).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AllocateMemoryRequest {
        /// Handle naming the device the memory is allocated on.
        pub device: Handle,
        /// Size of the allocation in bytes.
        pub size: u64,
    }

    impl AllocateMemoryRequest {
        /// Append the encoded body to `out`: a raw `u64` device handle followed
        /// by the `u64` allocation size.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
                size: r.u64()?,
            })
        }
    }

    /// Response body for [`vk_op::ALLOCATE_MEMORY`](super::vk_op::ALLOCATE_MEMORY).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AllocateMemoryResponse {
        /// Handle naming the newly allocated device memory.
        pub memory: Handle,
    }

    impl AllocateMemoryResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.memory.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                memory: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`vk_op::CREATE_BUFFER`](super::vk_op::CREATE_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateBufferRequest {
        /// Handle naming the device the buffer is created on.
        pub device: Handle,
        /// Size of the buffer in bytes.
        pub size: u64,
        /// Buffer usage flag bits.
        pub usage: u32,
    }

    impl CreateBufferRequest {
        /// Append the encoded body to `out`: a raw `u64` device handle, the
        /// `u64` buffer size, then the `u32` usage flags.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
            out.extend_from_slice(&self.usage.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
                size: r.u64()?,
                usage: r.u32()?,
            })
        }
    }

    /// Response body for [`vk_op::CREATE_BUFFER`](super::vk_op::CREATE_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateBufferResponse {
        /// Handle naming the newly created buffer.
        pub buffer: Handle,
    }

    impl CreateBufferResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.buffer.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                buffer: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`vk_op::BIND_BUFFER_MEMORY`](super::vk_op::BIND_BUFFER_MEMORY).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BindBufferMemoryRequest {
        /// Handle naming the buffer being bound.
        pub buffer: Handle,
        /// Handle naming the device memory bound to the buffer.
        pub memory: Handle,
        /// Offset into the memory at which the buffer is bound.
        pub offset: u64,
    }

    impl BindBufferMemoryRequest {
        /// Append the encoded body to `out`: a raw `u64` buffer handle, a raw
        /// `u64` memory handle, then the `u64` bind offset.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.buffer.raw().to_le_bytes());
            out.extend_from_slice(&self.memory.raw().to_le_bytes());
            out.extend_from_slice(&self.offset.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                buffer: Handle::from_raw(r.u64()?),
                memory: Handle::from_raw(r.u64()?),
                offset: r.u64()?,
            })
        }
    }

    /// Request body for
    /// [`vk_op::CREATE_COMMAND_POOL`](super::vk_op::CREATE_COMMAND_POOL).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateCommandPoolRequest {
        /// Handle naming the device the command pool is created on.
        pub device: Handle,
        /// Index of the queue family the pool's buffers are submitted to.
        pub queue_family_index: u32,
    }

    impl CreateCommandPoolRequest {
        /// Append the encoded body to `out`: a raw `u64` device handle followed
        /// by the `u32` queue family index.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
            out.extend_from_slice(&self.queue_family_index.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
                queue_family_index: r.u32()?,
            })
        }
    }

    /// Response body for
    /// [`vk_op::CREATE_COMMAND_POOL`](super::vk_op::CREATE_COMMAND_POOL).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateCommandPoolResponse {
        /// Handle naming the newly created command pool.
        pub pool: Handle,
    }

    impl CreateCommandPoolResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.pool.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                pool: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`vk_op::ALLOCATE_COMMAND_BUFFER`](super::vk_op::ALLOCATE_COMMAND_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AllocateCommandBufferRequest {
        /// Handle naming the command pool the buffer is allocated from.
        pub pool: Handle,
    }

    impl AllocateCommandBufferRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.pool.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                pool: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Response body for
    /// [`vk_op::ALLOCATE_COMMAND_BUFFER`](super::vk_op::ALLOCATE_COMMAND_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AllocateCommandBufferResponse {
        /// Handle naming the newly allocated command buffer.
        pub command_buffer: Handle,
    }

    impl AllocateCommandBufferResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.command_buffer.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                command_buffer: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`vk_op::QUEUE_SUBMIT`](super::vk_op::QUEUE_SUBMIT).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct QueueSubmitRequest {
        /// Handle naming the queue the command buffer is submitted to.
        pub queue: Handle,
        /// Handle naming the command buffer being submitted.
        pub command_buffer: Handle,
    }

    impl QueueSubmitRequest {
        /// Append the encoded body to `out`: a raw `u64` queue handle followed
        /// by the raw `u64` command buffer handle.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.queue.raw().to_le_bytes());
            out.extend_from_slice(&self.command_buffer.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                queue: Handle::from_raw(r.u64()?),
                command_buffer: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`vk_op::CMD_COPY_BUFFER`](super::vk_op::CMD_COPY_BUFFER).
    ///
    /// This records into a command buffer; the reply is an empty (ack) body, so
    /// there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CmdCopyBufferRequest {
        /// Handle naming the command buffer the copy is recorded into.
        pub command_buffer: Handle,
        /// Handle naming the source buffer copied from.
        pub src: Handle,
        /// Handle naming the destination buffer copied into.
        pub dst: Handle,
        /// Number of bytes copied.
        pub size: u64,
    }

    impl CmdCopyBufferRequest {
        /// Append the encoded body to `out`: a raw `u64` command buffer handle,
        /// a raw `u64` source handle, a raw `u64` destination handle, then the
        /// `u64` copy size.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.command_buffer.raw().to_le_bytes());
            out.extend_from_slice(&self.src.raw().to_le_bytes());
            out.extend_from_slice(&self.dst.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                command_buffer: Handle::from_raw(r.u64()?),
                src: Handle::from_raw(r.u64()?),
                dst: Handle::from_raw(r.u64()?),
                size: r.u64()?,
            })
        }
    }

    /// Request body for [`vk_op::CMD_DRAW`](super::vk_op::CMD_DRAW).
    ///
    /// This records into a command buffer; the reply is an empty (ack) body, so
    /// there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CmdDrawRequest {
        /// Handle naming the command buffer the draw is recorded into.
        pub command_buffer: Handle,
        /// Number of vertices to draw.
        pub vertex_count: u32,
        /// Number of instances to draw.
        pub instance_count: u32,
    }

    impl CmdDrawRequest {
        /// Append the encoded body to `out`: a raw `u64` command buffer handle
        /// followed by the `u32` vertex count and `u32` instance count.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.command_buffer.raw().to_le_bytes());
            out.extend_from_slice(&self.vertex_count.to_le_bytes());
            out.extend_from_slice(&self.instance_count.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                command_buffer: Handle::from_raw(r.u64()?),
                vertex_count: r.u32()?,
                instance_count: r.u32()?,
            })
        }
    }
}

/// OpenGL request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the OpenGL opcodes in [`gl_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
///
/// [`gl_op::CREATE_CONTEXT`](super::gl_op::CREATE_CONTEXT) takes an empty
/// request body, and [`gl_op::MAKE_CURRENT`](super::gl_op::MAKE_CURRENT)
/// replies with an empty (ack) body, so neither needs a struct here.
pub mod gl {
    use super::{Handle, ProtocolError, Reader};

    /// Response body for [`gl_op::CREATE_CONTEXT`](super::gl_op::CREATE_CONTEXT).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateContextResponse {
        /// Handle naming the newly created context.
        pub context: Handle,
    }

    impl CreateContextResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`gl_op::MAKE_CURRENT`](super::gl_op::MAKE_CURRENT).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MakeCurrentRequest {
        /// Handle naming the context to make current.
        pub context: Handle,
    }

    impl MakeCurrentRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`gl_op::GEN_BUFFER`](super::gl_op::GEN_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GenBufferRequest {
        /// Handle naming the context the buffer is generated in.
        pub context: Handle,
    }

    impl GenBufferRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Response body for [`gl_op::GEN_BUFFER`](super::gl_op::GEN_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct GenBufferResponse {
        /// Handle naming the newly generated buffer.
        pub buffer: Handle,
    }

    impl GenBufferResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.buffer.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                buffer: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// CUDA request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the CUDA opcodes in [`cuda_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
///
/// [`cuda_op::MEM_FREE`](super::cuda_op::MEM_FREE) replies with an empty (ack)
/// body, so its response needs no struct here.
pub mod cuda {
    use super::{Handle, ProtocolError, Reader};

    /// Request body for [`cuda_op::CTX_CREATE`](super::cuda_op::CTX_CREATE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CtxCreateRequest {
        /// Ordinal of the device the context is created on.
        pub device_ordinal: u32,
    }

    impl CtxCreateRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device_ordinal.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device_ordinal: r.u32()?,
            })
        }
    }

    /// Response body for [`cuda_op::CTX_CREATE`](super::cuda_op::CTX_CREATE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CtxCreateResponse {
        /// Handle naming the newly created context.
        pub context: Handle,
    }

    impl CtxCreateResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`cuda_op::MEM_ALLOC`](super::cuda_op::MEM_ALLOC).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemAllocRequest {
        /// Handle naming the context the memory is allocated in.
        pub context: Handle,
        /// Size of the allocation in bytes.
        pub size: u64,
    }

    impl MemAllocRequest {
        /// Append the encoded body to `out`: a raw `u64` context handle followed
        /// by the `u64` allocation size.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
                size: r.u64()?,
            })
        }
    }

    /// Response body for [`cuda_op::MEM_ALLOC`](super::cuda_op::MEM_ALLOC).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemAllocResponse {
        /// Handle naming the newly allocated device pointer.
        pub dptr: Handle,
    }

    impl MemAllocResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.dptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                dptr: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`cuda_op::MEM_FREE`](super::cuda_op::MEM_FREE).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemFreeRequest {
        /// Handle naming the device pointer being freed.
        pub dptr: Handle,
    }

    impl MemFreeRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.dptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                dptr: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// HIP request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the HIP opcodes in [`hip_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
///
/// [`hip_op::FREE`](super::hip_op::FREE) replies with an empty (ack) body, and
/// [`hip_op::STREAM_CREATE`](super::hip_op::STREAM_CREATE) takes an empty
/// request body, so neither needs a struct here.
pub mod hip {
    use super::{Handle, ProtocolError, Reader};

    /// Request body for [`hip_op::MALLOC`](super::hip_op::MALLOC).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MallocRequest {
        /// Size of the allocation in bytes.
        pub size: u64,
    }

    impl MallocRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self { size: r.u64()? })
        }
    }

    /// Response body for [`hip_op::MALLOC`](super::hip_op::MALLOC).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MallocResponse {
        /// Handle naming the newly allocated device pointer.
        pub dptr: Handle,
    }

    impl MallocResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.dptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                dptr: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`hip_op::FREE`](super::hip_op::FREE).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FreeRequest {
        /// Handle naming the device pointer being freed.
        pub dptr: Handle,
    }

    impl FreeRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.dptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                dptr: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Response body for
    /// [`hip_op::STREAM_CREATE`](super::hip_op::STREAM_CREATE).
    ///
    /// The request body is empty, so there is no request struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StreamCreateResponse {
        /// Handle naming the newly created stream.
        pub stream: Handle,
    }

    impl StreamCreateResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.stream.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                stream: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// OpenCL request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the OpenCL opcodes in [`cl_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
///
/// [`cl_op::CREATE_CONTEXT`](super::cl_op::CREATE_CONTEXT) takes an empty
/// request body, and [`cl_op::RELEASE_BUFFER`](super::cl_op::RELEASE_BUFFER)
/// replies with an empty (ack) body, so neither needs a struct here.
pub mod cl {
    use super::{Handle, ProtocolError, Reader};

    /// Response body for
    /// [`cl_op::CREATE_CONTEXT`](super::cl_op::CREATE_CONTEXT).
    ///
    /// The request body is empty, so there is no request struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateContextResponse {
        /// Handle naming the newly created context.
        pub context: Handle,
    }

    impl CreateContextResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`cl_op::CREATE_BUFFER`](super::cl_op::CREATE_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateBufferRequest {
        /// Handle naming the context the buffer is created in.
        pub context: Handle,
        /// Size of the buffer in bytes.
        pub size: u64,
    }

    impl CreateBufferRequest {
        /// Append the encoded body to `out`: a raw `u64` context handle followed
        /// by the `u64` buffer size.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
                size: r.u64()?,
            })
        }
    }

    /// Response body for [`cl_op::CREATE_BUFFER`](super::cl_op::CREATE_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateBufferResponse {
        /// Handle naming the newly created memory buffer.
        pub mem: Handle,
    }

    impl CreateBufferResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.mem.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                mem: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`cl_op::RELEASE_BUFFER`](super::cl_op::RELEASE_BUFFER).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ReleaseBufferRequest {
        /// Handle naming the memory buffer being released.
        pub mem: Handle,
    }

    impl ReleaseBufferRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.mem.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                mem: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// Level Zero request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the Level Zero opcodes in
/// [`l0_op`]. Every multi-byte field is little-endian and a [`Handle`] is
/// carried as its raw 64-bit value (see [`Handle::raw`]). Short buffers decode
/// to [`ProtocolError::UnexpectedEof`].
///
/// [`l0_op::CONTEXT_CREATE`](super::l0_op::CONTEXT_CREATE) takes an empty
/// request body, and [`l0_op::MEM_FREE`](super::l0_op::MEM_FREE) replies with
/// an empty (ack) body, so neither needs a struct here.
pub mod l0 {
    use super::{Handle, ProtocolError, Reader};

    /// Response body for
    /// [`l0_op::CONTEXT_CREATE`](super::l0_op::CONTEXT_CREATE).
    ///
    /// The request body is empty, so there is no request struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ContextCreateResponse {
        /// Handle naming the newly created context.
        pub context: Handle,
    }

    impl ContextCreateResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`l0_op::MEM_ALLOC_DEVICE`](super::l0_op::MEM_ALLOC_DEVICE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemAllocDeviceRequest {
        /// Handle naming the context the memory is allocated in.
        pub context: Handle,
        /// Size of the allocation in bytes.
        pub size: u64,
    }

    impl MemAllocDeviceRequest {
        /// Append the encoded body to `out`: a raw `u64` context handle followed
        /// by the `u64` allocation size.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
                size: r.u64()?,
            })
        }
    }

    /// Response body for
    /// [`l0_op::MEM_ALLOC_DEVICE`](super::l0_op::MEM_ALLOC_DEVICE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemAllocDeviceResponse {
        /// Handle naming the newly allocated device pointer.
        pub ptr: Handle,
    }

    impl MemAllocDeviceResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.ptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                ptr: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`l0_op::MEM_FREE`](super::l0_op::MEM_FREE).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MemFreeRequest {
        /// Handle naming the device pointer being freed.
        pub ptr: Handle,
    }

    impl MemFreeRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.ptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                ptr: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// Video codec request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the Video opcodes in
/// [`video_op`]. Every multi-byte field is little-endian and a [`Handle`] is
/// carried as its raw 64-bit value (see [`Handle::raw`]). Short buffers decode
/// to [`ProtocolError::UnexpectedEof`].
///
/// [`video_op::DESTROY_SESSION`](super::video_op::DESTROY_SESSION) replies with
/// an empty (ack) body, so its response needs no struct here.
pub mod video {
    use super::{Handle, ProtocolError, Reader};

    /// Request body for
    /// [`video_op::CREATE_DECODE_SESSION`](super::video_op::CREATE_DECODE_SESSION).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateDecodeSessionRequest {
        /// Codec identifier the session decodes.
        pub codec: u32,
        /// Decoded frame width in pixels.
        pub width: u32,
        /// Decoded frame height in pixels.
        pub height: u32,
    }

    impl CreateDecodeSessionRequest {
        /// Append the encoded body to `out`: the `u32` codec id followed by the
        /// `u32` width and `u32` height.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.codec.to_le_bytes());
            out.extend_from_slice(&self.width.to_le_bytes());
            out.extend_from_slice(&self.height.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                codec: r.u32()?,
                width: r.u32()?,
                height: r.u32()?,
            })
        }
    }

    /// Response body for
    /// [`video_op::CREATE_DECODE_SESSION`](super::video_op::CREATE_DECODE_SESSION).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateDecodeSessionResponse {
        /// Handle naming the newly created decode session.
        pub session: Handle,
    }

    impl CreateDecodeSessionResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.session.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                session: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`video_op::DECODE_FRAME`](super::video_op::DECODE_FRAME).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DecodeFrameRequest {
        /// Handle naming the decode session the bitstream is submitted to.
        pub session: Handle,
        /// Length of the coded bitstream in bytes.
        pub bitstream_len: u32,
    }

    impl DecodeFrameRequest {
        /// Append the encoded body to `out`: a raw `u64` session handle followed
        /// by the `u32` bitstream length.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.session.raw().to_le_bytes());
            out.extend_from_slice(&self.bitstream_len.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                session: Handle::from_raw(r.u64()?),
                bitstream_len: r.u32()?,
            })
        }
    }

    /// Response body for [`video_op::DECODE_FRAME`](super::video_op::DECODE_FRAME).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DecodeFrameResponse {
        /// Index of the decoded frame in the session's output sequence.
        pub frame_index: u32,
    }

    impl DecodeFrameResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.frame_index.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                frame_index: r.u32()?,
            })
        }
    }

    /// Request body for
    /// [`video_op::DESTROY_SESSION`](super::video_op::DESTROY_SESSION).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DestroySessionRequest {
        /// Handle naming the decode session being destroyed.
        pub session: Handle,
    }

    impl DestroySessionRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.session.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                session: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// WebGPU request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the WebGPU opcodes in
/// [`wgpu_op`]. Every multi-byte field is little-endian and a [`Handle`] is
/// carried as its raw 64-bit value (see [`Handle::raw`]). Short buffers decode
/// to [`ProtocolError::UnexpectedEof`].
///
/// [`wgpu_op::REQUEST_DEVICE`](super::wgpu_op::REQUEST_DEVICE) takes an empty
/// request body, and
/// [`wgpu_op::DESTROY_BUFFER`](super::wgpu_op::DESTROY_BUFFER) replies with an
/// empty (ack) body, so neither needs a struct here.
pub mod wgpu {
    use super::{Handle, ProtocolError, Reader};

    /// Response body for
    /// [`wgpu_op::REQUEST_DEVICE`](super::wgpu_op::REQUEST_DEVICE).
    ///
    /// The request body is empty, so there is no request struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct RequestDeviceResponse {
        /// Handle naming the newly acquired device.
        pub device: Handle,
    }

    impl RequestDeviceResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`wgpu_op::CREATE_BUFFER`](super::wgpu_op::CREATE_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateBufferRequest {
        /// Handle naming the device the buffer is created on.
        pub device: Handle,
        /// Size of the buffer in bytes.
        pub size: u64,
        /// Buffer usage flag bits.
        pub usage: u32,
    }

    impl CreateBufferRequest {
        /// Append the encoded body to `out`: a raw `u64` device handle, the
        /// `u64` buffer size, then the `u32` usage flags.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.device.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
            out.extend_from_slice(&self.usage.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                device: Handle::from_raw(r.u64()?),
                size: r.u64()?,
                usage: r.u32()?,
            })
        }
    }

    /// Response body for
    /// [`wgpu_op::CREATE_BUFFER`](super::wgpu_op::CREATE_BUFFER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateBufferResponse {
        /// Handle naming the newly created buffer.
        pub buffer: Handle,
    }

    impl CreateBufferResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.buffer.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                buffer: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`wgpu_op::DESTROY_BUFFER`](super::wgpu_op::DESTROY_BUFFER).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DestroyBufferRequest {
        /// Handle naming the buffer being destroyed.
        pub buffer: Handle,
    }

    impl DestroyBufferRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.buffer.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                buffer: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// OptiX request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the OptiX opcodes in
/// [`optix_op`]. Every multi-byte field is little-endian and a [`Handle`] is
/// carried as its raw 64-bit value (see [`Handle::raw`]). Short buffers decode
/// to [`ProtocolError::UnexpectedEof`].
///
/// [`optix_op::CONTEXT_CREATE`](super::optix_op::CONTEXT_CREATE) takes an empty
/// request body, and [`optix_op::DESTROY`](super::optix_op::DESTROY) replies
/// with an empty (ack) body, so neither needs a struct here.
pub mod optix {
    use super::{Handle, ProtocolError, Reader};

    /// Response body for
    /// [`optix_op::CONTEXT_CREATE`](super::optix_op::CONTEXT_CREATE).
    ///
    /// The request body is empty, so there is no request struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ContextCreateResponse {
        /// Handle naming the newly created context.
        pub context: Handle,
    }

    impl ContextCreateResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`optix_op::PIPELINE_CREATE`](super::optix_op::PIPELINE_CREATE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PipelineCreateRequest {
        /// Handle naming the context the pipeline is created in.
        pub context: Handle,
    }

    impl PipelineCreateRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.context.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                context: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Response body for
    /// [`optix_op::PIPELINE_CREATE`](super::optix_op::PIPELINE_CREATE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PipelineCreateResponse {
        /// Handle naming the newly created pipeline.
        pub pipeline: Handle,
    }

    impl PipelineCreateResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.pipeline.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                pipeline: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`optix_op::DESTROY`](super::optix_op::DESTROY).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DestroyRequest {
        /// Handle naming the context or pipeline being destroyed.
        pub handle: Handle,
    }

    impl DestroyRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.handle.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                handle: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// SYCL request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the SYCL opcodes in [`sycl_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
///
/// [`sycl_op::QUEUE_CREATE`](super::sycl_op::QUEUE_CREATE) takes an empty
/// request body, and [`sycl_op::FREE`](super::sycl_op::FREE) replies with an
/// empty (ack) body, so neither needs a struct here.
pub mod sycl {
    use super::{Handle, ProtocolError, Reader};

    /// Response body for
    /// [`sycl_op::QUEUE_CREATE`](super::sycl_op::QUEUE_CREATE).
    ///
    /// The request body is empty, so there is no request struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct QueueCreateResponse {
        /// Handle naming the newly created queue.
        pub queue: Handle,
    }

    impl QueueCreateResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.queue.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                queue: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for
    /// [`sycl_op::MALLOC_DEVICE`](super::sycl_op::MALLOC_DEVICE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MallocDeviceRequest {
        /// Handle naming the queue the memory is allocated on.
        pub queue: Handle,
        /// Size of the allocation in bytes.
        pub size: u64,
    }

    impl MallocDeviceRequest {
        /// Append the encoded body to `out`: a raw `u64` queue handle followed
        /// by the `u64` allocation size.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.queue.raw().to_le_bytes());
            out.extend_from_slice(&self.size.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                queue: Handle::from_raw(r.u64()?),
                size: r.u64()?,
            })
        }
    }

    /// Response body for
    /// [`sycl_op::MALLOC_DEVICE`](super::sycl_op::MALLOC_DEVICE).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct MallocDeviceResponse {
        /// Handle naming the newly allocated device pointer.
        pub ptr: Handle,
    }

    impl MallocDeviceResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.ptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                ptr: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`sycl_op::FREE`](super::sycl_op::FREE).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FreeRequest {
        /// Handle naming the device pointer being freed.
        pub ptr: Handle,
    }

    impl FreeRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.ptr.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                ptr: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// AMF request/response body encoders and decoders.
///
/// These match the canonical wire bodies for the AMF opcodes in [`amf_op`].
/// Every multi-byte field is little-endian and a [`Handle`] is carried as its
/// raw 64-bit value (see [`Handle::raw`]). Short buffers decode to
/// [`ProtocolError::UnexpectedEof`].
///
/// [`amf_op::DESTROY_ENCODER`](super::amf_op::DESTROY_ENCODER) replies with an
/// empty (ack) body, so its response needs no struct here.
pub mod amf {
    use super::{Handle, ProtocolError, Reader};

    /// Request body for
    /// [`amf_op::CREATE_ENCODER`](super::amf_op::CREATE_ENCODER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateEncoderRequest {
        /// Codec identifier the encoder produces.
        pub codec: u32,
        /// Encoded frame width in pixels.
        pub width: u32,
        /// Encoded frame height in pixels.
        pub height: u32,
    }

    impl CreateEncoderRequest {
        /// Append the encoded body to `out`: the `u32` codec id followed by the
        /// `u32` width and `u32` height.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.codec.to_le_bytes());
            out.extend_from_slice(&self.width.to_le_bytes());
            out.extend_from_slice(&self.height.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                codec: r.u32()?,
                width: r.u32()?,
                height: r.u32()?,
            })
        }
    }

    /// Response body for
    /// [`amf_op::CREATE_ENCODER`](super::amf_op::CREATE_ENCODER).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CreateEncoderResponse {
        /// Handle naming the newly created encoder.
        pub encoder: Handle,
    }

    impl CreateEncoderResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.encoder.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                encoder: Handle::from_raw(r.u64()?),
            })
        }
    }

    /// Request body for [`amf_op::ENCODE_FRAME`](super::amf_op::ENCODE_FRAME).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EncodeFrameRequest {
        /// Handle naming the encoder the frame is submitted to.
        pub encoder: Handle,
        /// Length of the raw input frame in bytes.
        pub frame_len: u32,
    }

    impl EncodeFrameRequest {
        /// Append the encoded body to `out`: a raw `u64` encoder handle followed
        /// by the `u32` frame length.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.encoder.raw().to_le_bytes());
            out.extend_from_slice(&self.frame_len.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                encoder: Handle::from_raw(r.u64()?),
                frame_len: r.u32()?,
            })
        }
    }

    /// Response body for [`amf_op::ENCODE_FRAME`](super::amf_op::ENCODE_FRAME).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EncodeFrameResponse {
        /// Length of the produced coded packet in bytes.
        pub packet_len: u32,
    }

    impl EncodeFrameResponse {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.packet_len.to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                packet_len: r.u32()?,
            })
        }
    }

    /// Request body for
    /// [`amf_op::DESTROY_ENCODER`](super::amf_op::DESTROY_ENCODER).
    ///
    /// The reply is an empty (ack) body, so there is no response struct.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DestroyEncoderRequest {
        /// Handle naming the encoder being destroyed.
        pub encoder: Handle,
    }

    impl DestroyEncoderRequest {
        /// Append the encoded body to `out`.
        pub fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.encoder.raw().to_le_bytes());
        }

        /// Decode a body.
        pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
            let mut r = Reader::new(buf);
            Ok(Self {
                encoder: Handle::from_raw(r.u64()?),
            })
        }
    }
}

/// The kind of a control-plane frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum FrameKind {
    /// A client-issued request.
    Request = 0,
    /// A server reply correlated by `req_id`.
    Response = 1,
    /// An unsolicited server → client event (e.g. async completion).
    Event = 2,
}

impl FrameKind {
    /// Decode a frame kind from its wire value.
    pub fn from_u16(v: u16) -> Result<Self, ProtocolError> {
        match v {
            0 => Ok(Self::Request),
            1 => Ok(Self::Response),
            2 => Ok(Self::Event),
            other => Err(ProtocolError::BadKind(other)),
        }
    }
}

/// Fixed-size protocol frame header.
///
/// `req_id` is the request/response correlation id; `seq` is the per-session
/// monotonic ordering/fence sequence — two distinct ids with distinct roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// Protocol major version of the sender.
    pub version: u16,
    /// Reserved flag bits (none defined yet).
    pub flags: u16,
    /// Frame kind.
    pub kind: FrameKind,
    /// Entrypoint opcode (see [`opcode`]).
    pub opcode: u32,
    /// Request/response correlation id.
    pub req_id: u32,
    /// Per-session monotonic ordering/fence sequence.
    pub seq: u64,
    /// Length of the payload that follows this header, in bytes.
    pub body_len: u32,
}

/// Encoded size of a [`FrameHeader`] on the wire, in bytes.
///
/// `version(2) + flags(2) + kind(2) + reserved(2) + opcode(4) + req_id(4) +
/// seq(8) + body_len(4)`.
pub const HEADER_LEN: usize = 28;

impl FrameHeader {
    /// Append the encoded header to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&(self.kind as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // reserved
        out.extend_from_slice(&self.opcode.to_le_bytes());
        out.extend_from_slice(&self.req_id.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.body_len.to_le_bytes());
    }

    /// Decode a header from the front of `buf`.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader::new(buf);
        let version = r.u16()?;
        let flags = r.u16()?;
        let kind = FrameKind::from_u16(r.u16()?)?;
        let _reserved = r.u16()?;
        let opcode = r.u32()?;
        let req_id = r.u32()?;
        let seq = r.u64()?;
        let body_len = r.u32()?;
        Ok(Self {
            version,
            flags,
            kind,
            opcode,
            req_id,
            seq,
            body_len,
        })
    }
}

/// Handshake request body (client → server).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    /// Highest protocol major the client supports.
    pub proto_major: u16,
    /// Highest protocol minor the client supports.
    pub proto_minor: u16,
    /// Optional feature bitset the client requests.
    pub features: u32,
    /// Largest control-frame body the client is willing to receive.
    pub max_frame_body: u32,
}

impl Hello {
    /// Append the encoded body to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.proto_major.to_le_bytes());
        out.extend_from_slice(&self.proto_minor.to_le_bytes());
        out.extend_from_slice(&self.features.to_le_bytes());
        out.extend_from_slice(&self.max_frame_body.to_le_bytes());
    }

    /// Decode a body.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader::new(buf);
        Ok(Self {
            proto_major: r.u16()?,
            proto_minor: r.u16()?,
            features: r.u32()?,
            max_frame_body: r.u32()?,
        })
    }
}

/// Handshake response body (server → client).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Welcome {
    /// Protocol major the server selected.
    pub proto_major: u16,
    /// Protocol minor the server selected.
    pub proto_minor: u16,
    /// Feature bitset the server granted.
    pub features: u32,
    /// Negotiated maximum control-frame body.
    pub max_frame_body: u32,
    /// Server-assigned session id.
    pub session_id: u64,
}

impl Welcome {
    /// Append the encoded body to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.proto_major.to_le_bytes());
        out.extend_from_slice(&self.proto_minor.to_le_bytes());
        out.extend_from_slice(&self.features.to_le_bytes());
        out.extend_from_slice(&self.max_frame_body.to_le_bytes());
        out.extend_from_slice(&self.session_id.to_le_bytes());
    }

    /// Decode a body.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader::new(buf);
        Ok(Self {
            proto_major: r.u16()?,
            proto_minor: r.u16()?,
            features: r.u32()?,
            max_frame_body: r.u32()?,
            session_id: r.u64()?,
        })
    }
}

/// Encode a complete protocol frame (header + body) into a fresh buffer.
pub fn encode_frame(header: &FrameHeader, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    header.encode(&mut out);
    out.extend_from_slice(body);
    out
}

/// Decode a complete protocol frame, returning the header and a slice of the
/// body that follows it. The caller validates `body_len` against the slice.
pub fn decode_frame(buf: &[u8]) -> Result<(FrameHeader, &[u8]), ProtocolError> {
    if buf.len() < HEADER_LEN {
        return Err(ProtocolError::UnexpectedEof);
    }
    let header = FrameHeader::decode(buf)?;
    Ok((header, &buf[HEADER_LEN..]))
}

/// A 64-bit wire handle that names a server-side resource (decision D5).
///
/// Layout (most-significant bit first):
///
/// ```text
///  bits 56..64  bits 32..56   bits 0..32
/// ┌───────────┬─────────────┬──────────────┐
/// │ kind (8)  │ gen (24)    │ slot idx (32)│
/// └───────────┴─────────────┴──────────────┘
/// ```
///
/// The `generation` field guards against use-after-free of a reused slot: each
/// time a slot is reallocated its generation is bumped, so a stale handle
/// carrying the old generation no longer matches. When a slot's generation
/// would exceed [`Handle::GENERATION_MAX`] the slot is *retired* and never
/// reused, so the generation never wraps back to a value an old handle holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle(u64);

impl Handle {
    /// Number of bits in the `kind` field (the top byte).
    pub const KIND_BITS: u32 = 8;
    /// Number of bits in the `generation` field.
    pub const GENERATION_BITS: u32 = 24;
    /// Number of bits in the `slot` index field (the low word).
    pub const SLOT_BITS: u32 = 32;

    /// Largest representable generation value (`2^24 - 1`). A slot whose
    /// generation would exceed this is retired rather than reused.
    pub const GENERATION_MAX: u32 = (1 << Self::GENERATION_BITS) - 1;

    /// Pack a `kind`, `generation`, and `slot` index into a wire handle.
    ///
    /// `generation` is masked to [`Handle::GENERATION_BITS`] bits; any high bits
    /// are discarded so the packed value always round-trips through
    /// [`Handle::generation`].
    pub const fn new(kind: u8, generation: u32, slot: u32) -> Handle {
        let kind = (kind as u64) << (Self::GENERATION_BITS + Self::SLOT_BITS);
        let generation = ((generation & Self::GENERATION_MAX) as u64) << Self::SLOT_BITS;
        let slot = slot as u64;
        Handle(kind | generation | slot)
    }

    /// The `kind` byte (the top 8 bits).
    pub const fn kind(self) -> u8 {
        (self.0 >> (Self::GENERATION_BITS + Self::SLOT_BITS)) as u8
    }

    /// The 24-bit `generation` counter.
    pub const fn generation(self) -> u32 {
        ((self.0 >> Self::SLOT_BITS) as u32) & Self::GENERATION_MAX
    }

    /// The 32-bit `slot` index (the low word).
    pub const fn slot(self) -> u32 {
        self.0 as u32
    }

    /// The raw 64-bit wire value.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Wrap a raw 64-bit wire value.
    pub const fn from_raw(raw: u64) -> Handle {
        Handle(raw)
    }
}

/// A bounds-checked little-endian reader over a byte slice.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let end = self
            .pos
            .checked_add(N)
            .ok_or(ProtocolError::UnexpectedEof)?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(ProtocolError::UnexpectedEof)?;
        self.pos = end;
        slice.try_into().map_err(|_| ProtocolError::UnexpectedEof)
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }

    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opcode_pack_unpack() {
        let op = opcode(ApiId::Vulkan, 0x0042);
        assert_eq!(op, 0x0100_0042);
        assert_eq!(opcode_api(op), ApiId::Vulkan as u8);
        assert_eq!(opcode_call(op), 0x0042);
    }

    #[test]
    fn core_opcodes_are_in_core_namespace() {
        assert_eq!(opcode_api(core_op::HELLO), ApiId::Core as u8);
        assert_eq!(opcode_api(core_op::NOOP), ApiId::Core as u8);
    }

    #[test]
    fn vk_opcodes_are_in_vulkan_namespace() {
        assert_eq!(opcode_api(vk_op::CREATE_INSTANCE), ApiId::Vulkan as u8);
        assert_ne!(vk_op::CREATE_INSTANCE, vk_op::DESTROY_INSTANCE);
    }

    #[test]
    fn gl_opcodes_are_in_opengl_namespace() {
        assert_eq!(opcode_api(gl_op::CREATE_CONTEXT), ApiId::OpenGl as u8);
        assert_eq!(opcode_api(gl_op::MAKE_CURRENT), ApiId::OpenGl as u8);
        assert_eq!(opcode_api(gl_op::GEN_BUFFER), ApiId::OpenGl as u8);
        assert_ne!(gl_op::CREATE_CONTEXT, gl_op::MAKE_CURRENT);
        assert_ne!(gl_op::MAKE_CURRENT, gl_op::GEN_BUFFER);
    }

    #[test]
    fn frame_header_roundtrip() {
        let h = FrameHeader {
            version: PROTOCOL_MAJOR,
            flags: 0,
            kind: FrameKind::Request,
            opcode: core_op::NOOP,
            req_id: 7,
            seq: 99,
            body_len: 0,
        };
        let mut buf = Vec::new();
        h.encode(&mut buf);
        assert_eq!(buf.len(), HEADER_LEN);
        let got = FrameHeader::decode(&buf).expect("decode");
        assert_eq!(got, h);
    }

    #[test]
    fn hello_welcome_roundtrip() {
        let hello = Hello {
            proto_major: 0,
            proto_minor: 0,
            features: 0,
            max_frame_body: DEFAULT_MAX_FRAME_BODY,
        };
        let mut b = Vec::new();
        hello.encode(&mut b);
        assert_eq!(Hello::decode(&b).expect("hello"), hello);

        let welcome = Welcome {
            proto_major: 0,
            proto_minor: 0,
            features: 0,
            max_frame_body: DEFAULT_MAX_FRAME_BODY,
            session_id: 0xDEAD_BEEF,
        };
        let mut b = Vec::new();
        welcome.encode(&mut b);
        assert_eq!(Welcome::decode(&b).expect("welcome"), welcome);
    }

    #[test]
    fn decode_frame_rejects_short_buffer() {
        assert_eq!(decode_frame(&[0u8; 4]), Err(ProtocolError::UnexpectedEof));
    }

    #[test]
    fn handle_roundtrip() {
        let cases = [
            (0u8, 0u32, 0u32),
            (1, 1, 1),
            (0x7F, 0x00AB_CDEF & Handle::GENERATION_MAX, 0x1234_5678),
            (0xFF, Handle::GENERATION_MAX, u32::MAX),
            (0xFF, 0, 0),
            (0, Handle::GENERATION_MAX, 0),
            (0, 0, u32::MAX),
        ];
        for (kind, generation, slot) in cases {
            let h = Handle::new(kind, generation, slot);
            assert_eq!(h.kind(), kind, "kind for {h:?}");
            assert_eq!(h.generation(), generation, "generation for {h:?}");
            assert_eq!(h.slot(), slot, "slot for {h:?}");
            assert_eq!(Handle::from_raw(h.raw()), h, "raw roundtrip for {h:?}");
        }
    }

    #[test]
    fn handle_generation_masked_to_24_bits() {
        // Generation bits above bit 24 must be discarded, not bleed into kind.
        let h = Handle::new(0xAB, 0xFFFF_FFFF, 0x9999_9999);
        assert_eq!(h.generation(), Handle::GENERATION_MAX);
        assert_eq!(h.kind(), 0xAB);
        assert_eq!(h.slot(), 0x9999_9999);

        // A generation one past the max wraps to 0 after masking.
        let h = Handle::new(0x12, Handle::GENERATION_MAX + 1, 0x0000_0001);
        assert_eq!(h.generation(), 0);
        assert_eq!(h.kind(), 0x12);
        assert_eq!(h.slot(), 1);
    }

    #[test]
    fn handle_field_constants() {
        assert_eq!(Handle::KIND_BITS, 8);
        assert_eq!(Handle::GENERATION_BITS, 24);
        assert_eq!(Handle::SLOT_BITS, 32);
        assert_eq!(
            Handle::KIND_BITS + Handle::GENERATION_BITS + Handle::SLOT_BITS,
            64
        );
        assert_eq!(Handle::GENERATION_MAX, (1 << 24) - 1);
    }

    #[test]
    fn vk_create_instance_roundtrip() {
        let req = vk::CreateInstanceRequest {
            app_api_version: 0x0040_3000,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 4);
        assert_eq!(vk::CreateInstanceRequest::decode(&b).expect("req"), req);

        let resp = vk::CreateInstanceResponse {
            instance: Handle::new(1, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(vk::CreateInstanceResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn vk_enumerate_physical_devices_request_roundtrip() {
        let req = vk::EnumeratePhysicalDevicesRequest {
            instance: Handle::new(1, 3, 7),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            vk::EnumeratePhysicalDevicesRequest::decode(&b).expect("req"),
            req
        );
    }

    #[test]
    fn vk_enumerate_physical_devices_response_empty() {
        let resp = vk::EnumeratePhysicalDevicesResponse { devices: vec![] };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 4);
        assert_eq!(
            vk::EnumeratePhysicalDevicesResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn vk_enumerate_physical_devices_response_three() {
        let resp = vk::EnumeratePhysicalDevicesResponse {
            devices: vec![
                Handle::new(2, 0, 0),
                Handle::new(2, 1, 1),
                Handle::new(2, Handle::GENERATION_MAX, u32::MAX),
            ],
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 4 + 3 * 8);
        assert_eq!(
            vk::EnumeratePhysicalDevicesResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn vk_bodies_reject_short_buffers() {
        assert_eq!(
            vk::CreateInstanceRequest::decode(&[0u8; 3]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::CreateInstanceResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::EnumeratePhysicalDevicesRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        // Count says 2 devices but only one handle's worth of bytes follow.
        let mut truncated = Vec::new();
        truncated.extend_from_slice(&2u32.to_le_bytes());
        truncated.extend_from_slice(&0u64.to_le_bytes());
        assert_eq!(
            vk::EnumeratePhysicalDevicesResponse::decode(&truncated),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn vk_create_device_roundtrip() {
        let req = vk::CreateDeviceRequest {
            physical_device: Handle::new(2, 1, 9),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(vk::CreateDeviceRequest::decode(&b).expect("req"), req);

        let resp = vk::CreateDeviceResponse {
            device: Handle::new(3, 7, 11),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(vk::CreateDeviceResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn vk_get_device_queue_roundtrip() {
        let req = vk::GetDeviceQueueRequest {
            device: Handle::new(3, 2, 4),
            queue_family_index: 1,
            queue_index: u32::MAX,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(vk::GetDeviceQueueRequest::decode(&b).expect("req"), req);

        let resp = vk::GetDeviceQueueResponse {
            queue: Handle::new(4, Handle::GENERATION_MAX, 0),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(vk::GetDeviceQueueResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn vk_device_queue_bodies_reject_short_buffers() {
        assert_eq!(
            vk::CreateDeviceRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::CreateDeviceResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::GetDeviceQueueRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::GetDeviceQueueResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn gl_create_context_response_roundtrip() {
        let resp = gl::CreateContextResponse {
            context: Handle::new(10, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(gl::CreateContextResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn gl_make_current_request_roundtrip() {
        let req = gl::MakeCurrentRequest {
            context: Handle::new(10, 3, 7),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(gl::MakeCurrentRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn gl_gen_buffer_roundtrip() {
        let req = gl::GenBufferRequest {
            context: Handle::new(10, 2, 4),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(gl::GenBufferRequest::decode(&b).expect("req"), req);

        let resp = gl::GenBufferResponse {
            buffer: Handle::new(11, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(gl::GenBufferResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn gl_bodies_reject_short_buffers() {
        assert_eq!(
            gl::CreateContextResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            gl::MakeCurrentRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            gl::GenBufferRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            gl::GenBufferResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn vk_opcodes_memory_buffer_in_vulkan_namespace() {
        assert_eq!(opcode_api(vk_op::ALLOCATE_MEMORY), ApiId::Vulkan as u8);
        assert_eq!(opcode_api(vk_op::CREATE_BUFFER), ApiId::Vulkan as u8);
        assert_eq!(opcode_api(vk_op::BIND_BUFFER_MEMORY), ApiId::Vulkan as u8);
        assert_eq!(opcode_call(vk_op::ALLOCATE_MEMORY), 0x0010);
        assert_eq!(opcode_call(vk_op::CREATE_BUFFER), 0x0011);
        assert_eq!(opcode_call(vk_op::BIND_BUFFER_MEMORY), 0x0012);
    }

    #[test]
    fn vk_allocate_memory_roundtrip() {
        let req = vk::AllocateMemoryRequest {
            device: Handle::new(3, 2, 4),
            size: 0x1234_5678_9ABC_DEF0,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(vk::AllocateMemoryRequest::decode(&b).expect("req"), req);

        let resp = vk::AllocateMemoryResponse {
            memory: Handle::new(5, 7, 11),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(vk::AllocateMemoryResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn vk_create_buffer_roundtrip() {
        let req = vk::CreateBufferRequest {
            device: Handle::new(3, 1, 9),
            size: u64::MAX,
            usage: 0xDEAD_BEEF,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 20);
        assert_eq!(vk::CreateBufferRequest::decode(&b).expect("req"), req);

        let resp = vk::CreateBufferResponse {
            buffer: Handle::new(6, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(vk::CreateBufferResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn vk_bind_buffer_memory_roundtrip() {
        let req = vk::BindBufferMemoryRequest {
            buffer: Handle::new(6, 3, 2),
            memory: Handle::new(5, 4, 8),
            offset: 0x0FED_CBA9_8765_4321,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 24);
        assert_eq!(vk::BindBufferMemoryRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn vk_memory_buffer_bodies_reject_short_buffers() {
        assert_eq!(
            vk::AllocateMemoryRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::AllocateMemoryResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::CreateBufferRequest::decode(&[0u8; 19]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::CreateBufferResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::BindBufferMemoryRequest::decode(&[0u8; 23]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn vk_opcodes_command_buffer_in_vulkan_namespace() {
        assert_eq!(opcode_api(vk_op::CREATE_COMMAND_POOL), ApiId::Vulkan as u8);
        assert_eq!(
            opcode_api(vk_op::ALLOCATE_COMMAND_BUFFER),
            ApiId::Vulkan as u8
        );
        assert_eq!(opcode_api(vk_op::QUEUE_SUBMIT), ApiId::Vulkan as u8);
        assert_eq!(opcode_call(vk_op::CREATE_COMMAND_POOL), 0x0020);
        assert_eq!(opcode_call(vk_op::ALLOCATE_COMMAND_BUFFER), 0x0021);
        assert_eq!(opcode_call(vk_op::QUEUE_SUBMIT), 0x0022);
        assert_ne!(vk_op::CREATE_COMMAND_POOL, vk_op::ALLOCATE_COMMAND_BUFFER);
        assert_ne!(vk_op::ALLOCATE_COMMAND_BUFFER, vk_op::QUEUE_SUBMIT);
    }

    #[test]
    fn vk_create_command_pool_roundtrip() {
        let req = vk::CreateCommandPoolRequest {
            device: Handle::new(3, 2, 4),
            queue_family_index: u32::MAX,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 12);
        assert_eq!(vk::CreateCommandPoolRequest::decode(&b).expect("req"), req);

        let resp = vk::CreateCommandPoolResponse {
            pool: Handle::new(7, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            vk::CreateCommandPoolResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn vk_allocate_command_buffer_roundtrip() {
        let req = vk::AllocateCommandBufferRequest {
            pool: Handle::new(7, 3, 9),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            vk::AllocateCommandBufferRequest::decode(&b).expect("req"),
            req
        );

        let resp = vk::AllocateCommandBufferResponse {
            command_buffer: Handle::new(8, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            vk::AllocateCommandBufferResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn vk_queue_submit_roundtrip() {
        let req = vk::QueueSubmitRequest {
            queue: Handle::new(4, 1, 2),
            command_buffer: Handle::new(8, 6, 5),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(vk::QueueSubmitRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn vk_command_buffer_bodies_reject_short_buffers() {
        assert_eq!(
            vk::CreateCommandPoolRequest::decode(&[0u8; 11]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::CreateCommandPoolResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::AllocateCommandBufferRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::AllocateCommandBufferResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::QueueSubmitRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn vk_opcodes_draw_copy_in_vulkan_namespace() {
        assert_eq!(opcode_api(vk_op::CMD_COPY_BUFFER), ApiId::Vulkan as u8);
        assert_eq!(opcode_api(vk_op::CMD_DRAW), ApiId::Vulkan as u8);
        assert_eq!(opcode_call(vk_op::CMD_COPY_BUFFER), 0x0023);
        assert_eq!(opcode_call(vk_op::CMD_DRAW), 0x0024);
        assert_ne!(vk_op::CMD_COPY_BUFFER, vk_op::CMD_DRAW);
        assert_ne!(vk_op::QUEUE_SUBMIT, vk_op::CMD_COPY_BUFFER);
    }

    #[test]
    fn vk_cmd_copy_buffer_roundtrip() {
        let req = vk::CmdCopyBufferRequest {
            command_buffer: Handle::new(8, 1, 2),
            src: Handle::new(6, 3, 4),
            dst: Handle::new(6, 5, 6),
            size: u64::MAX,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 32);
        assert_eq!(vk::CmdCopyBufferRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn vk_cmd_draw_roundtrip() {
        let req = vk::CmdDrawRequest {
            command_buffer: Handle::new(8, 7, 9),
            vertex_count: u32::MAX,
            instance_count: 1,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(vk::CmdDrawRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn vk_draw_copy_bodies_reject_short_buffers() {
        assert_eq!(
            vk::CmdCopyBufferRequest::decode(&[0u8; 31]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            vk::CmdDrawRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn cuda_opcodes_are_in_cuda_namespace() {
        assert_eq!(opcode_api(cuda_op::CTX_CREATE), ApiId::Cuda as u8);
        assert_eq!(opcode_api(cuda_op::MEM_ALLOC), ApiId::Cuda as u8);
        assert_eq!(opcode_api(cuda_op::MEM_FREE), ApiId::Cuda as u8);
        assert_eq!(opcode_call(cuda_op::CTX_CREATE), 0x0001);
        assert_eq!(opcode_call(cuda_op::MEM_ALLOC), 0x0002);
        assert_eq!(opcode_call(cuda_op::MEM_FREE), 0x0003);
        assert_ne!(cuda_op::CTX_CREATE, cuda_op::MEM_ALLOC);
        assert_ne!(cuda_op::MEM_ALLOC, cuda_op::MEM_FREE);
    }

    #[test]
    fn cuda_ctx_create_roundtrip() {
        let req = cuda::CtxCreateRequest {
            device_ordinal: 0xDEAD_BEEF,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 4);
        assert_eq!(cuda::CtxCreateRequest::decode(&b).expect("req"), req);

        let resp = cuda::CtxCreateResponse {
            context: Handle::new(7, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(cuda::CtxCreateResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn cuda_mem_alloc_roundtrip() {
        let req = cuda::MemAllocRequest {
            context: Handle::new(7, 2, 4),
            size: 0x1234_5678_9ABC_DEF0,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(cuda::MemAllocRequest::decode(&b).expect("req"), req);

        let resp = cuda::MemAllocResponse {
            dptr: Handle::new(8, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(cuda::MemAllocResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn cuda_mem_free_roundtrip() {
        let req = cuda::MemFreeRequest {
            dptr: Handle::new(8, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(cuda::MemFreeRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn cuda_bodies_reject_short_buffers() {
        assert_eq!(
            cuda::CtxCreateRequest::decode(&[0u8; 3]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cuda::CtxCreateResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cuda::MemAllocRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cuda::MemAllocResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cuda::MemFreeRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn hip_opcodes_are_in_hip_namespace() {
        assert_eq!(opcode_api(hip_op::MALLOC), ApiId::Hip as u8);
        assert_eq!(opcode_api(hip_op::FREE), ApiId::Hip as u8);
        assert_eq!(opcode_api(hip_op::STREAM_CREATE), ApiId::Hip as u8);
        assert_eq!(opcode_call(hip_op::MALLOC), 0x0001);
        assert_eq!(opcode_call(hip_op::FREE), 0x0002);
        assert_eq!(opcode_call(hip_op::STREAM_CREATE), 0x0003);
        assert_ne!(hip_op::MALLOC, hip_op::FREE);
        assert_ne!(hip_op::FREE, hip_op::STREAM_CREATE);
    }

    #[test]
    fn hip_malloc_roundtrip() {
        let req = hip::MallocRequest {
            size: 0x1234_5678_9ABC_DEF0,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(hip::MallocRequest::decode(&b).expect("req"), req);

        let resp = hip::MallocResponse {
            dptr: Handle::new(8, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(hip::MallocResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn hip_free_roundtrip() {
        let req = hip::FreeRequest {
            dptr: Handle::new(8, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(hip::FreeRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn hip_stream_create_roundtrip() {
        let resp = hip::StreamCreateResponse {
            stream: Handle::new(9, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(hip::StreamCreateResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn hip_bodies_reject_short_buffers() {
        assert_eq!(
            hip::MallocRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            hip::MallocResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            hip::FreeRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            hip::StreamCreateResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn cl_opcodes_are_in_opencl_namespace() {
        assert_eq!(opcode_api(cl_op::CREATE_CONTEXT), ApiId::OpenCl as u8);
        assert_eq!(opcode_api(cl_op::CREATE_BUFFER), ApiId::OpenCl as u8);
        assert_eq!(opcode_api(cl_op::RELEASE_BUFFER), ApiId::OpenCl as u8);
        assert_eq!(opcode_call(cl_op::CREATE_CONTEXT), 0x0001);
        assert_eq!(opcode_call(cl_op::CREATE_BUFFER), 0x0002);
        assert_eq!(opcode_call(cl_op::RELEASE_BUFFER), 0x0003);
        assert_ne!(cl_op::CREATE_CONTEXT, cl_op::CREATE_BUFFER);
        assert_ne!(cl_op::CREATE_BUFFER, cl_op::RELEASE_BUFFER);
    }

    #[test]
    fn cl_create_context_response_roundtrip() {
        let resp = cl::CreateContextResponse {
            context: Handle::new(12, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(cl::CreateContextResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn cl_create_buffer_roundtrip() {
        let req = cl::CreateBufferRequest {
            context: Handle::new(12, 2, 4),
            size: 0x1234_5678_9ABC_DEF0,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(cl::CreateBufferRequest::decode(&b).expect("req"), req);

        let resp = cl::CreateBufferResponse {
            mem: Handle::new(13, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(cl::CreateBufferResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn cl_release_buffer_roundtrip() {
        let req = cl::ReleaseBufferRequest {
            mem: Handle::new(13, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(cl::ReleaseBufferRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn cl_bodies_reject_short_buffers() {
        assert_eq!(
            cl::CreateContextResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cl::CreateBufferRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cl::CreateBufferResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            cl::ReleaseBufferRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn l0_opcodes_are_in_level_zero_namespace() {
        assert_eq!(opcode_api(l0_op::CONTEXT_CREATE), ApiId::LevelZero as u8);
        assert_eq!(opcode_api(l0_op::MEM_ALLOC_DEVICE), ApiId::LevelZero as u8);
        assert_eq!(opcode_api(l0_op::MEM_FREE), ApiId::LevelZero as u8);
        assert_eq!(opcode_call(l0_op::CONTEXT_CREATE), 0x0001);
        assert_eq!(opcode_call(l0_op::MEM_ALLOC_DEVICE), 0x0002);
        assert_eq!(opcode_call(l0_op::MEM_FREE), 0x0003);
        assert_ne!(l0_op::CONTEXT_CREATE, l0_op::MEM_ALLOC_DEVICE);
        assert_ne!(l0_op::MEM_ALLOC_DEVICE, l0_op::MEM_FREE);
    }

    #[test]
    fn l0_context_create_response_roundtrip() {
        let resp = l0::ContextCreateResponse {
            context: Handle::new(14, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(l0::ContextCreateResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn l0_mem_alloc_device_roundtrip() {
        let req = l0::MemAllocDeviceRequest {
            context: Handle::new(14, 2, 4),
            size: 0x1234_5678_9ABC_DEF0,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(l0::MemAllocDeviceRequest::decode(&b).expect("req"), req);

        let resp = l0::MemAllocDeviceResponse {
            ptr: Handle::new(15, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(l0::MemAllocDeviceResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn l0_mem_free_roundtrip() {
        let req = l0::MemFreeRequest {
            ptr: Handle::new(15, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(l0::MemFreeRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn l0_bodies_reject_short_buffers() {
        assert_eq!(
            l0::ContextCreateResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            l0::MemAllocDeviceRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            l0::MemAllocDeviceResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            l0::MemFreeRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn full_frame_roundtrip() {
        let body = [1u8, 2, 3, 4];
        let h = FrameHeader {
            version: 0,
            flags: 0,
            kind: FrameKind::Response,
            opcode: core_op::NOOP,
            req_id: 1,
            seq: 1,
            body_len: body.len() as u32,
        };
        let frame = encode_frame(&h, &body);
        let (gh, gb) = decode_frame(&frame).expect("decode");
        assert_eq!(gh, h);
        assert_eq!(gb, &body);
    }

    #[test]
    fn video_opcodes_are_in_video_namespace() {
        assert_eq!(
            opcode_api(video_op::CREATE_DECODE_SESSION),
            ApiId::Video as u8
        );
        assert_eq!(opcode_api(video_op::DECODE_FRAME), ApiId::Video as u8);
        assert_eq!(opcode_api(video_op::DESTROY_SESSION), ApiId::Video as u8);
        assert_eq!(opcode_call(video_op::CREATE_DECODE_SESSION), 0x0001);
        assert_eq!(opcode_call(video_op::DECODE_FRAME), 0x0002);
        assert_eq!(opcode_call(video_op::DESTROY_SESSION), 0x0003);
        assert_ne!(video_op::CREATE_DECODE_SESSION, video_op::DECODE_FRAME);
        assert_ne!(video_op::DECODE_FRAME, video_op::DESTROY_SESSION);
    }

    #[test]
    fn video_create_decode_session_roundtrip() {
        let req = video::CreateDecodeSessionRequest {
            codec: 0xDEAD_BEEF,
            width: 1920,
            height: 1080,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 12);
        assert_eq!(
            video::CreateDecodeSessionRequest::decode(&b).expect("req"),
            req
        );

        let resp = video::CreateDecodeSessionResponse {
            session: Handle::new(16, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            video::CreateDecodeSessionResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn video_decode_frame_roundtrip() {
        let req = video::DecodeFrameRequest {
            session: Handle::new(16, 2, 4),
            bitstream_len: u32::MAX,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 12);
        assert_eq!(video::DecodeFrameRequest::decode(&b).expect("req"), req);

        let resp = video::DecodeFrameResponse {
            frame_index: 0x1234_5678,
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 4);
        assert_eq!(video::DecodeFrameResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn video_destroy_session_roundtrip() {
        let req = video::DestroySessionRequest {
            session: Handle::new(16, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(video::DestroySessionRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn video_bodies_reject_short_buffers() {
        assert_eq!(
            video::CreateDecodeSessionRequest::decode(&[0u8; 11]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            video::CreateDecodeSessionResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            video::DecodeFrameRequest::decode(&[0u8; 11]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            video::DecodeFrameResponse::decode(&[0u8; 3]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            video::DestroySessionRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn webgpu_api_id_value() {
        assert_eq!(ApiId::WebGpu as u8, 0x08);
    }

    #[test]
    fn wgpu_opcodes_are_in_webgpu_namespace() {
        assert_eq!(opcode_api(wgpu_op::REQUEST_DEVICE), ApiId::WebGpu as u8);
        assert_eq!(opcode_api(wgpu_op::CREATE_BUFFER), ApiId::WebGpu as u8);
        assert_eq!(opcode_api(wgpu_op::DESTROY_BUFFER), ApiId::WebGpu as u8);
        assert_eq!(opcode_call(wgpu_op::REQUEST_DEVICE), 0x0001);
        assert_eq!(opcode_call(wgpu_op::CREATE_BUFFER), 0x0002);
        assert_eq!(opcode_call(wgpu_op::DESTROY_BUFFER), 0x0003);
        assert_ne!(wgpu_op::REQUEST_DEVICE, wgpu_op::CREATE_BUFFER);
        assert_ne!(wgpu_op::CREATE_BUFFER, wgpu_op::DESTROY_BUFFER);
    }

    #[test]
    fn wgpu_request_device_response_roundtrip() {
        let resp = wgpu::RequestDeviceResponse {
            device: Handle::new(17, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(wgpu::RequestDeviceResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn wgpu_create_buffer_roundtrip() {
        let req = wgpu::CreateBufferRequest {
            device: Handle::new(17, 1, 9),
            size: u64::MAX,
            usage: 0xDEAD_BEEF,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 20);
        assert_eq!(wgpu::CreateBufferRequest::decode(&b).expect("req"), req);

        let resp = wgpu::CreateBufferResponse {
            buffer: Handle::new(18, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(wgpu::CreateBufferResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn wgpu_destroy_buffer_roundtrip() {
        let req = wgpu::DestroyBufferRequest {
            buffer: Handle::new(18, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(wgpu::DestroyBufferRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn wgpu_bodies_reject_short_buffers() {
        assert_eq!(
            wgpu::RequestDeviceResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            wgpu::CreateBufferRequest::decode(&[0u8; 19]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            wgpu::CreateBufferResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            wgpu::DestroyBufferRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn optix_api_id_value() {
        assert_eq!(ApiId::OptiX as u8, 0x09);
    }

    #[test]
    fn optix_opcodes_are_in_optix_namespace() {
        assert_eq!(opcode_api(optix_op::CONTEXT_CREATE), ApiId::OptiX as u8);
        assert_eq!(opcode_api(optix_op::PIPELINE_CREATE), ApiId::OptiX as u8);
        assert_eq!(opcode_api(optix_op::DESTROY), ApiId::OptiX as u8);
        assert_eq!(opcode_api(optix_op::CONTEXT_CREATE), 0x09);
        assert_eq!(opcode_call(optix_op::CONTEXT_CREATE), 0x0001);
        assert_eq!(opcode_call(optix_op::PIPELINE_CREATE), 0x0002);
        assert_eq!(opcode_call(optix_op::DESTROY), 0x0003);
        assert_ne!(optix_op::CONTEXT_CREATE, optix_op::PIPELINE_CREATE);
        assert_ne!(optix_op::PIPELINE_CREATE, optix_op::DESTROY);
    }

    #[test]
    fn optix_context_create_response_roundtrip() {
        let resp = optix::ContextCreateResponse {
            context: Handle::new(19, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            optix::ContextCreateResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn optix_pipeline_create_roundtrip() {
        let req = optix::PipelineCreateRequest {
            context: Handle::new(19, 2, 4),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(optix::PipelineCreateRequest::decode(&b).expect("req"), req);

        let resp = optix::PipelineCreateResponse {
            pipeline: Handle::new(20, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(
            optix::PipelineCreateResponse::decode(&b).expect("resp"),
            resp
        );
    }

    #[test]
    fn optix_destroy_roundtrip() {
        let req = optix::DestroyRequest {
            handle: Handle::new(19, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(optix::DestroyRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn optix_bodies_reject_short_buffers() {
        assert_eq!(
            optix::ContextCreateResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            optix::PipelineCreateRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            optix::PipelineCreateResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            optix::DestroyRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn sycl_api_id_value() {
        assert_eq!(ApiId::Sycl as u8, 0x0A);
    }

    #[test]
    fn sycl_opcodes_are_in_sycl_namespace() {
        assert_eq!(opcode_api(sycl_op::QUEUE_CREATE), ApiId::Sycl as u8);
        assert_eq!(opcode_api(sycl_op::MALLOC_DEVICE), ApiId::Sycl as u8);
        assert_eq!(opcode_api(sycl_op::FREE), ApiId::Sycl as u8);
        assert_eq!(opcode_api(sycl_op::QUEUE_CREATE), 0x0A);
        assert_eq!(opcode_call(sycl_op::QUEUE_CREATE), 0x0001);
        assert_eq!(opcode_call(sycl_op::MALLOC_DEVICE), 0x0002);
        assert_eq!(opcode_call(sycl_op::FREE), 0x0003);
        assert_ne!(sycl_op::QUEUE_CREATE, sycl_op::MALLOC_DEVICE);
        assert_ne!(sycl_op::MALLOC_DEVICE, sycl_op::FREE);
    }

    #[test]
    fn sycl_queue_create_response_roundtrip() {
        let resp = sycl::QueueCreateResponse {
            queue: Handle::new(21, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(sycl::QueueCreateResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn sycl_malloc_device_roundtrip() {
        let req = sycl::MallocDeviceRequest {
            queue: Handle::new(21, 2, 4),
            size: 0x1234_5678_9ABC_DEF0,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 16);
        assert_eq!(sycl::MallocDeviceRequest::decode(&b).expect("req"), req);

        let resp = sycl::MallocDeviceResponse {
            ptr: Handle::new(22, Handle::GENERATION_MAX, u32::MAX),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(sycl::MallocDeviceResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn sycl_free_roundtrip() {
        let req = sycl::FreeRequest {
            ptr: Handle::new(22, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(sycl::FreeRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn sycl_bodies_reject_short_buffers() {
        assert_eq!(
            sycl::QueueCreateResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            sycl::MallocDeviceRequest::decode(&[0u8; 15]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            sycl::MallocDeviceResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            sycl::FreeRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }

    #[test]
    fn amf_api_id_value() {
        assert_eq!(ApiId::Amf as u8, 0x0B);
    }

    #[test]
    fn amf_opcodes_are_in_amf_namespace() {
        assert_eq!(opcode_api(amf_op::CREATE_ENCODER), ApiId::Amf as u8);
        assert_eq!(opcode_api(amf_op::ENCODE_FRAME), ApiId::Amf as u8);
        assert_eq!(opcode_api(amf_op::DESTROY_ENCODER), ApiId::Amf as u8);
        assert_eq!(opcode_api(amf_op::CREATE_ENCODER), 0x0B);
        assert_eq!(opcode_call(amf_op::CREATE_ENCODER), 0x0001);
        assert_eq!(opcode_call(amf_op::ENCODE_FRAME), 0x0002);
        assert_eq!(opcode_call(amf_op::DESTROY_ENCODER), 0x0003);
        assert_ne!(amf_op::CREATE_ENCODER, amf_op::ENCODE_FRAME);
        assert_ne!(amf_op::ENCODE_FRAME, amf_op::DESTROY_ENCODER);
    }

    #[test]
    fn amf_create_encoder_roundtrip() {
        let req = amf::CreateEncoderRequest {
            codec: 0xDEAD_BEEF,
            width: 1920,
            height: 1080,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 12);
        assert_eq!(amf::CreateEncoderRequest::decode(&b).expect("req"), req);

        let resp = amf::CreateEncoderResponse {
            encoder: Handle::new(23, 5, 42),
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(amf::CreateEncoderResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn amf_encode_frame_roundtrip() {
        let req = amf::EncodeFrameRequest {
            encoder: Handle::new(23, 2, 4),
            frame_len: u32::MAX,
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 12);
        assert_eq!(amf::EncodeFrameRequest::decode(&b).expect("req"), req);

        let resp = amf::EncodeFrameResponse {
            packet_len: 0x1234_5678,
        };
        let mut b = Vec::new();
        resp.encode(&mut b);
        assert_eq!(b.len(), 4);
        assert_eq!(amf::EncodeFrameResponse::decode(&b).expect("resp"), resp);
    }

    #[test]
    fn amf_destroy_encoder_roundtrip() {
        let req = amf::DestroyEncoderRequest {
            encoder: Handle::new(23, 3, 11),
        };
        let mut b = Vec::new();
        req.encode(&mut b);
        assert_eq!(b.len(), 8);
        assert_eq!(amf::DestroyEncoderRequest::decode(&b).expect("req"), req);
    }

    #[test]
    fn amf_bodies_reject_short_buffers() {
        assert_eq!(
            amf::CreateEncoderRequest::decode(&[0u8; 11]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            amf::CreateEncoderResponse::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            amf::EncodeFrameRequest::decode(&[0u8; 11]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            amf::EncodeFrameResponse::decode(&[0u8; 3]),
            Err(ProtocolError::UnexpectedEof)
        );
        assert_eq!(
            amf::DestroyEncoderRequest::decode(&[0u8; 7]),
            Err(ProtocolError::UnexpectedEof)
        );
    }
}
