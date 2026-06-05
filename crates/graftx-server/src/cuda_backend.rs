//! CUDA backend dispatch.
//!
//! The [`Session`](crate::Session) routes any CUDA-namespace opcode to this
//! backend. Like the Vulkan and OpenGL backends, the CUDA backend is a pure-Rust
//! **stub**: it tracks object lifetimes in generational handle tables but
//! performs no real driver work. No CUDA toolkit, GPU, or FFI dependency is
//! involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a CUDA context handle.
const KIND_CUDA_CONTEXT: u8 = 20;
/// Server object kind for a CUDA device-pointer handle.
const KIND_CUDA_DEVICE_PTR: u8 = 21;

/// Server-side state tracked for one created CUDA context.
#[derive(Debug)]
struct CudaCtxState {
    /// Ordinal of the device this context was created on. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    device_ordinal: u32,
}

/// Server-side state tracked for one CUDA allocation.
#[derive(Debug)]
struct CudaMemState {
    /// The context handle this allocation was made in. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    context: proto::Handle,
    /// Size of the allocation in bytes.
    #[allow(dead_code)]
    size: u64,
}

/// Pure-Rust CUDA backend stub.
///
/// Owns generational handle tables for the CUDA objects it tracks. It answers
/// [`cuda_op::CTX_CREATE`](proto::cuda_op::CTX_CREATE) by minting a context
/// handle, [`cuda_op::MEM_ALLOC`](proto::cuda_op::MEM_ALLOC) by minting a
/// device-pointer handle parented to a known context, and
/// [`cuda_op::MEM_FREE`](proto::cuda_op::MEM_FREE) by removing a known
/// device-pointer handle and acknowledging with an empty body. The real driver
/// bridge lands in a later milestone; every other CUDA call is reported as
/// not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct CudaBackend {
    contexts: HandleTable<CudaCtxState>,
    allocations: HandleTable<CudaMemState>,
}

impl CudaBackend {
    /// Create a new CUDA backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contexts: HandleTable::new(),
            allocations: HandleTable::new(),
        }
    }
}

impl Backend for CudaBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::Cuda
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the CUDA namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Cuda as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::cuda_op::CTX_CREATE => {
                let req = proto::cuda::CtxCreateRequest::decode(body)?;
                let context = self.contexts.insert(
                    KIND_CUDA_CONTEXT,
                    CudaCtxState {
                        device_ordinal: req.device_ordinal,
                    },
                );
                let mut out = Vec::new();
                proto::cuda::CtxCreateResponse { context }.encode(&mut out);
                Ok(out)
            }
            proto::cuda_op::MEM_ALLOC => {
                let req = proto::cuda::MemAllocRequest::decode(body)?;
                // The context must have been created on this backend.
                if self.contexts.get(req.context).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let dptr = self.allocations.insert(
                    KIND_CUDA_DEVICE_PTR,
                    CudaMemState {
                        context: req.context,
                        size: req.size,
                    },
                );
                let mut out = Vec::new();
                proto::cuda::MemAllocResponse { dptr }.encode(&mut out);
                Ok(out)
            }
            proto::cuda_op::MEM_FREE => {
                let req = proto::cuda::MemFreeRequest::decode(body)?;
                // The device pointer must have been allocated on this backend and
                // still be live; removing it both validates and frees it.
                if self.allocations.remove(req.dptr).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the CUDA namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `CTX_CREATE` request body for `device_ordinal`.
    fn ctx_create_body(device_ordinal: u32) -> Vec<u8> {
        let mut body = Vec::new();
        proto::cuda::CtxCreateRequest { device_ordinal }.encode(&mut body);
        body
    }

    /// Encode a `MEM_ALLOC` request body for `context` and `size`.
    fn mem_alloc_body(context: proto::Handle, size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        proto::cuda::MemAllocRequest { context, size }.encode(&mut body);
        body
    }

    /// Encode a `MEM_FREE` request body for `dptr`.
    fn mem_free_body(dptr: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::cuda::MemFreeRequest { dptr }.encode(&mut body);
        body
    }

    /// Drive a backend through `CTX_CREATE`, returning the minted context handle.
    fn create_context(backend: &mut CudaBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::cuda_op::CTX_CREATE, 1, &ctx_create_body(0))
            .expect("ctx create should succeed");
        proto::cuda::CtxCreateResponse::decode(&resp)
            .expect("decode ctx create response")
            .context
    }

    /// Drive a backend through `CTX_CREATE` then `MEM_ALLOC`, returning the
    /// minted device-pointer handle.
    fn alloc_one(backend: &mut CudaBackend) -> proto::Handle {
        let context = create_context(backend);
        let resp = backend
            .handle(proto::cuda_op::MEM_ALLOC, 2, &mem_alloc_body(context, 4096))
            .expect("mem alloc should succeed");
        proto::cuda::MemAllocResponse::decode(&resp)
            .expect("decode mem alloc response")
            .dptr
    }

    #[test]
    fn ctx_create_returns_kind_twenty() {
        let mut backend = CudaBackend::new();
        let resp = backend
            .handle(proto::cuda_op::CTX_CREATE, 1, &ctx_create_body(3))
            .expect("ctx create should succeed");

        let decoded = proto::cuda::CtxCreateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.context.kind(), KIND_CUDA_CONTEXT);
        // The returned handle must resolve in the context table.
        let state = backend
            .contexts
            .get(decoded.context)
            .expect("context state present");
        assert_eq!(state.device_ordinal, 3);
    }

    #[test]
    fn mem_alloc_after_ctx_returns_kind_twenty_one() {
        let mut backend = CudaBackend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(proto::cuda_op::MEM_ALLOC, 2, &mem_alloc_body(context, 1024))
            .expect("mem alloc should succeed");

        let decoded = proto::cuda::MemAllocResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.dptr.kind(), KIND_CUDA_DEVICE_PTR);
        let state = backend
            .allocations
            .get(decoded.dptr)
            .expect("allocation state present");
        assert_eq!(state.context.raw(), context.raw());
        assert_eq!(state.size, 1024);
    }

    #[test]
    fn mem_alloc_with_bogus_context_errors() {
        let mut backend = CudaBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_CUDA_CONTEXT, 0, 999);
        let err = backend
            .handle(proto::cuda_op::MEM_ALLOC, 1, &mem_alloc_body(bogus, 1024))
            .expect_err("mem alloc with bogus context must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn mem_free_of_live_dptr_succeeds() {
        let mut backend = CudaBackend::new();
        let dptr = alloc_one(&mut backend);

        let resp = backend
            .handle(proto::cuda_op::MEM_FREE, 3, &mem_free_body(dptr))
            .expect("mem free should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The allocation must no longer resolve.
        assert!(backend.allocations.get(dptr).is_none());
    }

    #[test]
    fn mem_free_of_bogus_dptr_errors() {
        let mut backend = CudaBackend::new();
        // A handle that was never allocated by this backend.
        let bogus = proto::Handle::new(KIND_CUDA_DEVICE_PTR, 0, 999);
        let err = backend
            .handle(proto::cuda_op::MEM_FREE, 1, &mem_free_body(bogus))
            .expect_err("mem free with bogus dptr must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn mem_free_of_already_freed_dptr_errors() {
        let mut backend = CudaBackend::new();
        let dptr = alloc_one(&mut backend);

        backend
            .handle(proto::cuda_op::MEM_FREE, 3, &mem_free_body(dptr))
            .expect("first mem free should succeed");

        let err = backend
            .handle(proto::cuda_op::MEM_FREE, 4, &mem_free_body(dptr))
            .expect_err("second mem free must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = CudaBackend::new();
        // Opcode 0 is a core opcode, not in the CUDA namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-cuda opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
