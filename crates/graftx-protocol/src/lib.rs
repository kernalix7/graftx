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
}
