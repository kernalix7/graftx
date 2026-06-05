//! WebGPU backend dispatch.
//!
//! The [`Session`](crate::Session) routes any WebGPU-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, and CUDA backends, the WebGPU backend is a
//! pure-Rust **stub**: it tracks object lifetimes in generational handle tables
//! but performs no real driver work. No `wgpu`, Dawn, browser, or FFI
//! dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a WebGPU device handle.
const KIND_WGPU_DEVICE: u8 = 70;
/// Server object kind for a WebGPU buffer handle.
const KIND_WGPU_BUFFER: u8 = 71;

/// Server-side state tracked for one requested WebGPU device.
///
/// The device carries no parameters yet; the unit-only field keeps the type a
/// non-zero-sized table entry and leaves room for negotiated limits in a later
/// milestone.
#[derive(Debug, Default)]
struct WgpuDevState;

/// Server-side state tracked for one created WebGPU buffer.
#[derive(Debug)]
struct WgpuBufState {
    /// The device handle this buffer was created on. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    device: proto::Handle,
    /// Size of the buffer in bytes.
    #[allow(dead_code)]
    size: u64,
    /// Buffer usage flag bits.
    #[allow(dead_code)]
    usage: u32,
}

/// Pure-Rust WebGPU backend stub.
///
/// Owns generational handle tables for the WebGPU objects it tracks. It answers
/// [`wgpu_op::REQUEST_DEVICE`](proto::wgpu_op::REQUEST_DEVICE) by minting a
/// device handle, [`wgpu_op::CREATE_BUFFER`](proto::wgpu_op::CREATE_BUFFER) by
/// minting a buffer handle parented to a known device, and
/// [`wgpu_op::DESTROY_BUFFER`](proto::wgpu_op::DESTROY_BUFFER) by removing a
/// known buffer handle and acknowledging with an empty body. The real driver
/// bridge lands in a later milestone; every other WebGPU call is reported as
/// not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct WebGpuBackend {
    devices: HandleTable<WgpuDevState>,
    buffers: HandleTable<WgpuBufState>,
}

impl WebGpuBackend {
    /// Create a new WebGPU backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            devices: HandleTable::new(),
            buffers: HandleTable::new(),
        }
    }
}

impl Backend for WebGpuBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::WebGpu
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the WebGPU namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::WebGpu as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::wgpu_op::REQUEST_DEVICE => {
                // The request body is empty; mint a fresh device handle.
                let device = self.devices.insert(KIND_WGPU_DEVICE, WgpuDevState);
                let mut out = Vec::new();
                proto::wgpu::RequestDeviceResponse { device }.encode(&mut out);
                Ok(out)
            }
            proto::wgpu_op::CREATE_BUFFER => {
                let req = proto::wgpu::CreateBufferRequest::decode(body)?;
                // The device must have been requested on this backend.
                if self.devices.get(req.device).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let buffer = self.buffers.insert(
                    KIND_WGPU_BUFFER,
                    WgpuBufState {
                        device: req.device,
                        size: req.size,
                        usage: req.usage,
                    },
                );
                let mut out = Vec::new();
                proto::wgpu::CreateBufferResponse { buffer }.encode(&mut out);
                Ok(out)
            }
            proto::wgpu_op::DESTROY_BUFFER => {
                let req = proto::wgpu::DestroyBufferRequest::decode(body)?;
                // The buffer must have been created on this backend and still be
                // live; removing it both validates and frees it.
                if self.buffers.remove(req.buffer).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the WebGPU namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `CREATE_BUFFER` request body for `device`, `size`, and `usage`.
    fn create_buffer_body(device: proto::Handle, size: u64, usage: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::wgpu::CreateBufferRequest {
            device,
            size,
            usage,
        }
        .encode(&mut body);
        body
    }

    /// Encode a `DESTROY_BUFFER` request body for `buffer`.
    fn destroy_buffer_body(buffer: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::wgpu::DestroyBufferRequest { buffer }.encode(&mut body);
        body
    }

    /// Drive a backend through `REQUEST_DEVICE`, returning the minted device
    /// handle.
    fn request_device(backend: &mut WebGpuBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::wgpu_op::REQUEST_DEVICE, 1, &[])
            .expect("request device should succeed");
        proto::wgpu::RequestDeviceResponse::decode(&resp)
            .expect("decode request device response")
            .device
    }

    /// Drive a backend through `REQUEST_DEVICE` then `CREATE_BUFFER`, returning
    /// the minted buffer handle.
    fn create_one_buffer(backend: &mut WebGpuBackend) -> proto::Handle {
        let device = request_device(backend);
        let resp = backend
            .handle(
                proto::wgpu_op::CREATE_BUFFER,
                2,
                &create_buffer_body(device, 4096, 0),
            )
            .expect("create buffer should succeed");
        proto::wgpu::CreateBufferResponse::decode(&resp)
            .expect("decode create buffer response")
            .buffer
    }

    #[test]
    fn request_device_returns_kind_seventy() {
        let mut backend = WebGpuBackend::new();
        let resp = backend
            .handle(proto::wgpu_op::REQUEST_DEVICE, 1, &[])
            .expect("request device should succeed");

        let decoded = proto::wgpu::RequestDeviceResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.device.kind(), KIND_WGPU_DEVICE);
        // The returned handle must resolve in the device table.
        assert!(backend.devices.get(decoded.device).is_some());
    }

    #[test]
    fn create_buffer_after_device_returns_kind_seventy_one() {
        let mut backend = WebGpuBackend::new();
        let device = request_device(&mut backend);

        let resp = backend
            .handle(
                proto::wgpu_op::CREATE_BUFFER,
                2,
                &create_buffer_body(device, 1024, 8),
            )
            .expect("create buffer should succeed");

        let decoded = proto::wgpu::CreateBufferResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.buffer.kind(), KIND_WGPU_BUFFER);
        let state = backend
            .buffers
            .get(decoded.buffer)
            .expect("buffer state present");
        assert_eq!(state.device.raw(), device.raw());
        assert_eq!(state.size, 1024);
        assert_eq!(state.usage, 8);
    }

    #[test]
    fn create_buffer_with_bogus_device_errors() {
        let mut backend = WebGpuBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_WGPU_DEVICE, 0, 999);
        let err = backend
            .handle(
                proto::wgpu_op::CREATE_BUFFER,
                1,
                &create_buffer_body(bogus, 1024, 0),
            )
            .expect_err("create buffer with bogus device must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_buffer_of_live_buffer_succeeds() {
        let mut backend = WebGpuBackend::new();
        let buffer = create_one_buffer(&mut backend);

        let resp = backend
            .handle(
                proto::wgpu_op::DESTROY_BUFFER,
                3,
                &destroy_buffer_body(buffer),
            )
            .expect("destroy buffer should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The buffer must no longer resolve.
        assert!(backend.buffers.get(buffer).is_none());
    }

    #[test]
    fn destroy_buffer_of_bogus_buffer_errors() {
        let mut backend = WebGpuBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_WGPU_BUFFER, 0, 999);
        let err = backend
            .handle(
                proto::wgpu_op::DESTROY_BUFFER,
                1,
                &destroy_buffer_body(bogus),
            )
            .expect_err("destroy buffer with bogus buffer must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_buffer_of_already_destroyed_buffer_errors() {
        let mut backend = WebGpuBackend::new();
        let buffer = create_one_buffer(&mut backend);

        backend
            .handle(
                proto::wgpu_op::DESTROY_BUFFER,
                3,
                &destroy_buffer_body(buffer),
            )
            .expect("first destroy buffer should succeed");

        let err = backend
            .handle(
                proto::wgpu_op::DESTROY_BUFFER,
                4,
                &destroy_buffer_body(buffer),
            )
            .expect_err("second destroy buffer must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = WebGpuBackend::new();
        // Opcode 0 is a core opcode, not in the WebGPU namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-webgpu opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
