//! Level Zero backend dispatch.
//!
//! The [`Session`](crate::Session) routes any Level Zero-namespace opcode to
//! this backend. Like the Vulkan, OpenGL, CUDA, HIP, and OpenCL backends, the
//! Level Zero backend is a pure-Rust **stub**: it tracks object lifetimes in
//! generational handle tables but performs no real driver work. No Level Zero
//! SDK, GPU, or FFI dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a Level Zero context handle.
const KIND_L0_CONTEXT: u8 = 50;
/// Server object kind for a Level Zero device-memory handle.
const KIND_L0_DEVICE_MEM: u8 = 51;

/// Server-side state tracked for one created Level Zero context.
#[derive(Debug, Default)]
struct L0CtxState {
    /// Marks the context as live; reserved for future per-context state.
    #[allow(dead_code)]
    created: bool,
}

/// Server-side state tracked for one Level Zero device-memory allocation.
#[derive(Debug)]
struct L0MemState {
    /// The context handle this allocation was made in. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    context: proto::Handle,
    /// Size of the allocation in bytes.
    #[allow(dead_code)]
    size: u64,
}

/// Pure-Rust Level Zero backend stub.
///
/// Owns generational handle tables for the Level Zero objects it tracks. It
/// answers [`l0_op::CONTEXT_CREATE`](proto::l0_op::CONTEXT_CREATE) by minting a
/// context handle, [`l0_op::MEM_ALLOC_DEVICE`](proto::l0_op::MEM_ALLOC_DEVICE)
/// by minting a device-memory handle parented to a known context, and
/// [`l0_op::MEM_FREE`](proto::l0_op::MEM_FREE) by removing a known device-memory
/// handle and acknowledging with an empty body. The real driver bridge lands in
/// a later milestone; every other Level Zero call is reported as not-yet-
/// implemented via [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct L0Backend {
    contexts: HandleTable<L0CtxState>,
    mems: HandleTable<L0MemState>,
}

impl L0Backend {
    /// Create a new Level Zero backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contexts: HandleTable::new(),
            mems: HandleTable::new(),
        }
    }
}

impl Backend for L0Backend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::LevelZero
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the Level Zero namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::LevelZero as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::l0_op::CONTEXT_CREATE => {
                // The request body is empty, so there is nothing to decode.
                let context = self
                    .contexts
                    .insert(KIND_L0_CONTEXT, L0CtxState { created: true });
                let mut out = Vec::new();
                proto::l0::ContextCreateResponse { context }.encode(&mut out);
                Ok(out)
            }
            proto::l0_op::MEM_ALLOC_DEVICE => {
                let req = proto::l0::MemAllocDeviceRequest::decode(body)?;
                // The context must have been created on this backend.
                if self.contexts.get(req.context).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let ptr = self.mems.insert(
                    KIND_L0_DEVICE_MEM,
                    L0MemState {
                        context: req.context,
                        size: req.size,
                    },
                );
                let mut out = Vec::new();
                proto::l0::MemAllocDeviceResponse { ptr }.encode(&mut out);
                Ok(out)
            }
            proto::l0_op::MEM_FREE => {
                let req = proto::l0::MemFreeRequest::decode(body)?;
                // The allocation must have been created on this backend and
                // still be live; removing it both validates and frees it.
                if self.mems.remove(req.ptr).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the Level Zero namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `MEM_ALLOC_DEVICE` request body for `context` and `size`.
    fn mem_alloc_body(context: proto::Handle, size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        proto::l0::MemAllocDeviceRequest { context, size }.encode(&mut body);
        body
    }

    /// Encode a `MEM_FREE` request body for `ptr`.
    fn mem_free_body(ptr: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::l0::MemFreeRequest { ptr }.encode(&mut body);
        body
    }

    /// Drive a backend through `CONTEXT_CREATE`, returning the minted context
    /// handle.
    fn create_context(backend: &mut L0Backend) -> proto::Handle {
        let resp = backend
            .handle(proto::l0_op::CONTEXT_CREATE, 1, &[])
            .expect("context create should succeed");
        proto::l0::ContextCreateResponse::decode(&resp)
            .expect("decode context create response")
            .context
    }

    /// Drive a backend through `CONTEXT_CREATE` then `MEM_ALLOC_DEVICE`,
    /// returning the minted device-memory handle.
    fn alloc_one(backend: &mut L0Backend) -> proto::Handle {
        let context = create_context(backend);
        let resp = backend
            .handle(
                proto::l0_op::MEM_ALLOC_DEVICE,
                2,
                &mem_alloc_body(context, 4096),
            )
            .expect("mem alloc should succeed");
        proto::l0::MemAllocDeviceResponse::decode(&resp)
            .expect("decode mem alloc response")
            .ptr
    }

    #[test]
    fn context_create_returns_kind_fifty() {
        let mut backend = L0Backend::new();
        let resp = backend
            .handle(proto::l0_op::CONTEXT_CREATE, 1, &[])
            .expect("context create should succeed");

        let decoded = proto::l0::ContextCreateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.context.kind(), KIND_L0_CONTEXT);
        // The returned handle must resolve in the context table.
        assert!(backend.contexts.get(decoded.context).is_some());
    }

    #[test]
    fn mem_alloc_after_context_returns_kind_fifty_one() {
        let mut backend = L0Backend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(
                proto::l0_op::MEM_ALLOC_DEVICE,
                2,
                &mem_alloc_body(context, 1024),
            )
            .expect("mem alloc should succeed");

        let decoded = proto::l0::MemAllocDeviceResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.ptr.kind(), KIND_L0_DEVICE_MEM);
        let state = backend.mems.get(decoded.ptr).expect("mem state present");
        assert_eq!(state.context.raw(), context.raw());
        assert_eq!(state.size, 1024);
    }

    #[test]
    fn mem_alloc_with_bogus_context_errors() {
        let mut backend = L0Backend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_L0_CONTEXT, 0, 999);
        let err = backend
            .handle(
                proto::l0_op::MEM_ALLOC_DEVICE,
                1,
                &mem_alloc_body(bogus, 1024),
            )
            .expect_err("mem alloc with bogus context must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn mem_free_of_live_ptr_succeeds() {
        let mut backend = L0Backend::new();
        let ptr = alloc_one(&mut backend);

        let resp = backend
            .handle(proto::l0_op::MEM_FREE, 3, &mem_free_body(ptr))
            .expect("mem free should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The allocation must no longer resolve.
        assert!(backend.mems.get(ptr).is_none());
    }

    #[test]
    fn mem_free_of_bogus_ptr_errors() {
        let mut backend = L0Backend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_L0_DEVICE_MEM, 0, 999);
        let err = backend
            .handle(proto::l0_op::MEM_FREE, 1, &mem_free_body(bogus))
            .expect_err("mem free with bogus ptr must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn mem_free_of_already_freed_ptr_errors() {
        let mut backend = L0Backend::new();
        let ptr = alloc_one(&mut backend);

        backend
            .handle(proto::l0_op::MEM_FREE, 3, &mem_free_body(ptr))
            .expect("first free should succeed");

        let err = backend
            .handle(proto::l0_op::MEM_FREE, 4, &mem_free_body(ptr))
            .expect_err("second free must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = L0Backend::new();
        // Opcode 0 is a core opcode, not in the Level Zero namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-level-zero opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
