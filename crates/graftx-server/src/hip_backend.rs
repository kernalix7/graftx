//! HIP backend dispatch.
//!
//! The [`Session`](crate::Session) routes any HIP-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, and CUDA backends, the HIP backend is a
//! pure-Rust **stub**: it tracks object lifetimes in generational handle tables
//! but performs no real driver work. No ROCm toolkit, GPU, or FFI dependency is
//! involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a HIP device-pointer handle.
const KIND_HIP_DEVICE_PTR: u8 = 30;
/// Server object kind for a HIP stream handle.
const KIND_HIP_STREAM: u8 = 31;

/// Server-side state tracked for one HIP allocation.
#[derive(Debug)]
struct HipMemState {
    /// Size of the allocation in bytes.
    #[allow(dead_code)]
    size: u64,
}

/// Server-side state tracked for one created HIP stream.
#[derive(Debug, Default)]
struct HipStreamState {}

/// Pure-Rust HIP backend stub.
///
/// Owns generational handle tables for the HIP objects it tracks. It answers
/// [`hip_op::MALLOC`](proto::hip_op::MALLOC) by minting a device-pointer
/// handle, [`hip_op::FREE`](proto::hip_op::FREE) by removing a known
/// device-pointer handle and acknowledging with an empty body, and
/// [`hip_op::STREAM_CREATE`](proto::hip_op::STREAM_CREATE) by minting a stream
/// handle. The real driver bridge lands in a later milestone; every other HIP
/// call is reported as not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct HipBackend {
    allocations: HandleTable<HipMemState>,
    streams: HandleTable<HipStreamState>,
}

impl HipBackend {
    /// Create a new HIP backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            allocations: HandleTable::new(),
            streams: HandleTable::new(),
        }
    }
}

impl Backend for HipBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::Hip
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the HIP namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Hip as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::hip_op::MALLOC => {
                let req = proto::hip::MallocRequest::decode(body)?;
                let dptr = self
                    .allocations
                    .insert(KIND_HIP_DEVICE_PTR, HipMemState { size: req.size });
                let mut out = Vec::new();
                proto::hip::MallocResponse { dptr }.encode(&mut out);
                Ok(out)
            }
            proto::hip_op::FREE => {
                let req = proto::hip::FreeRequest::decode(body)?;
                // The device pointer must have been allocated on this backend and
                // still be live; removing it both validates and frees it.
                if self.allocations.remove(req.dptr).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            proto::hip_op::STREAM_CREATE => {
                // The request body is empty; mint a fresh stream handle.
                let stream = self
                    .streams
                    .insert(KIND_HIP_STREAM, HipStreamState::default());
                let mut out = Vec::new();
                proto::hip::StreamCreateResponse { stream }.encode(&mut out);
                Ok(out)
            }
            // Everything else in the HIP namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `MALLOC` request body for `size`.
    fn malloc_body(size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        proto::hip::MallocRequest { size }.encode(&mut body);
        body
    }

    /// Encode a `FREE` request body for `dptr`.
    fn free_body(dptr: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::hip::FreeRequest { dptr }.encode(&mut body);
        body
    }

    /// Drive a backend through `MALLOC`, returning the minted device-pointer
    /// handle.
    fn alloc_one(backend: &mut HipBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::hip_op::MALLOC, 1, &malloc_body(4096))
            .expect("malloc should succeed");
        proto::hip::MallocResponse::decode(&resp)
            .expect("decode malloc response")
            .dptr
    }

    #[test]
    fn malloc_returns_kind_thirty() {
        let mut backend = HipBackend::new();
        let resp = backend
            .handle(proto::hip_op::MALLOC, 1, &malloc_body(1024))
            .expect("malloc should succeed");

        let decoded = proto::hip::MallocResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.dptr.kind(), KIND_HIP_DEVICE_PTR);
        // The returned handle must resolve in the allocation table.
        let state = backend
            .allocations
            .get(decoded.dptr)
            .expect("allocation state present");
        assert_eq!(state.size, 1024);
    }

    #[test]
    fn free_of_live_dptr_succeeds() {
        let mut backend = HipBackend::new();
        let dptr = alloc_one(&mut backend);

        let resp = backend
            .handle(proto::hip_op::FREE, 2, &free_body(dptr))
            .expect("free should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The allocation must no longer resolve.
        assert!(backend.allocations.get(dptr).is_none());
    }

    #[test]
    fn free_of_bogus_dptr_errors() {
        let mut backend = HipBackend::new();
        // A handle that was never allocated by this backend.
        let bogus = proto::Handle::new(KIND_HIP_DEVICE_PTR, 0, 999);
        let err = backend
            .handle(proto::hip_op::FREE, 1, &free_body(bogus))
            .expect_err("free with bogus dptr must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn free_of_already_freed_dptr_errors() {
        let mut backend = HipBackend::new();
        let dptr = alloc_one(&mut backend);

        backend
            .handle(proto::hip_op::FREE, 2, &free_body(dptr))
            .expect("first free should succeed");

        let err = backend
            .handle(proto::hip_op::FREE, 3, &free_body(dptr))
            .expect_err("second free must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn stream_create_returns_kind_thirty_one() {
        let mut backend = HipBackend::new();
        let resp = backend
            .handle(proto::hip_op::STREAM_CREATE, 1, &[])
            .expect("stream create should succeed");

        let decoded = proto::hip::StreamCreateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.stream.kind(), KIND_HIP_STREAM);
        // The returned handle must resolve in the stream table.
        assert!(backend.streams.get(decoded.stream).is_some());
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = HipBackend::new();
        // Opcode 0 is a core opcode, not in the HIP namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-hip opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
