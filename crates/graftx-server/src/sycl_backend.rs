//! SYCL backend dispatch.
//!
//! The [`Session`](crate::Session) routes any SYCL-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, CUDA, HIP, OpenCL, and Level Zero
//! backends, the SYCL backend is a pure-Rust **stub**: it tracks object
//! lifetimes in generational handle tables but performs no real driver work. No
//! SYCL runtime, GPU, or FFI dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a SYCL queue handle.
const KIND_SYCL_QUEUE: u8 = 90;
/// Server object kind for a SYCL device-pointer handle.
const KIND_SYCL_DEVICE_PTR: u8 = 91;

/// Server-side state tracked for one created SYCL queue.
#[derive(Debug, Default)]
struct SyclQueueState {
    /// Marks the queue as live; reserved for future per-queue state.
    #[allow(dead_code)]
    created: bool,
}

/// Server-side state tracked for one SYCL device allocation.
#[derive(Debug)]
struct SyclPtrState {
    /// The queue handle this allocation was made on. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    queue: proto::Handle,
    /// Size of the allocation in bytes.
    #[allow(dead_code)]
    size: u64,
}

/// Pure-Rust SYCL backend stub.
///
/// Owns generational handle tables for the SYCL objects it tracks. It answers
/// [`sycl_op::QUEUE_CREATE`](proto::sycl_op::QUEUE_CREATE) by minting a queue
/// handle, [`sycl_op::MALLOC_DEVICE`](proto::sycl_op::MALLOC_DEVICE) by minting
/// a device-pointer handle parented to a known queue, and
/// [`sycl_op::FREE`](proto::sycl_op::FREE) by removing a known device-pointer
/// handle and acknowledging with an empty body. The real driver bridge lands in
/// a later milestone; every other SYCL call is reported as not-yet-implemented
/// via [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct SyclBackend {
    queues: HandleTable<SyclQueueState>,
    ptrs: HandleTable<SyclPtrState>,
}

impl SyclBackend {
    /// Create a new SYCL backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: HandleTable::new(),
            ptrs: HandleTable::new(),
        }
    }
}

impl Backend for SyclBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::Sycl
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the SYCL namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::Sycl as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::sycl_op::QUEUE_CREATE => {
                // The request body is empty, so there is nothing to decode.
                let queue = self
                    .queues
                    .insert(KIND_SYCL_QUEUE, SyclQueueState { created: true });
                let mut out = Vec::new();
                proto::sycl::QueueCreateResponse { queue }.encode(&mut out);
                Ok(out)
            }
            proto::sycl_op::MALLOC_DEVICE => {
                let req = proto::sycl::MallocDeviceRequest::decode(body)?;
                // The queue must have been created on this backend.
                if self.queues.get(req.queue).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let ptr = self.ptrs.insert(
                    KIND_SYCL_DEVICE_PTR,
                    SyclPtrState {
                        queue: req.queue,
                        size: req.size,
                    },
                );
                let mut out = Vec::new();
                proto::sycl::MallocDeviceResponse { ptr }.encode(&mut out);
                Ok(out)
            }
            proto::sycl_op::FREE => {
                let req = proto::sycl::FreeRequest::decode(body)?;
                // The allocation must have been created on this backend and
                // still be live; removing it both validates and frees it.
                if self.ptrs.remove(req.ptr).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the SYCL namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `MALLOC_DEVICE` request body for `queue` and `size`.
    fn malloc_body(queue: proto::Handle, size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        proto::sycl::MallocDeviceRequest { queue, size }.encode(&mut body);
        body
    }

    /// Encode a `FREE` request body for `ptr`.
    fn free_body(ptr: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::sycl::FreeRequest { ptr }.encode(&mut body);
        body
    }

    /// Drive a backend through `QUEUE_CREATE`, returning the minted queue
    /// handle.
    fn create_queue(backend: &mut SyclBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::sycl_op::QUEUE_CREATE, 1, &[])
            .expect("queue create should succeed");
        proto::sycl::QueueCreateResponse::decode(&resp)
            .expect("decode queue create response")
            .queue
    }

    /// Drive a backend through `QUEUE_CREATE` then `MALLOC_DEVICE`, returning
    /// the minted device-pointer handle.
    fn alloc_one(backend: &mut SyclBackend) -> proto::Handle {
        let queue = create_queue(backend);
        let resp = backend
            .handle(proto::sycl_op::MALLOC_DEVICE, 2, &malloc_body(queue, 4096))
            .expect("malloc device should succeed");
        proto::sycl::MallocDeviceResponse::decode(&resp)
            .expect("decode malloc device response")
            .ptr
    }

    #[test]
    fn queue_create_returns_kind_ninety() {
        let mut backend = SyclBackend::new();
        let resp = backend
            .handle(proto::sycl_op::QUEUE_CREATE, 1, &[])
            .expect("queue create should succeed");

        let decoded = proto::sycl::QueueCreateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.queue.kind(), KIND_SYCL_QUEUE);
        // The returned handle must resolve in the queue table.
        assert!(backend.queues.get(decoded.queue).is_some());
    }

    #[test]
    fn malloc_after_queue_returns_kind_ninety_one() {
        let mut backend = SyclBackend::new();
        let queue = create_queue(&mut backend);

        let resp = backend
            .handle(proto::sycl_op::MALLOC_DEVICE, 2, &malloc_body(queue, 1024))
            .expect("malloc device should succeed");

        let decoded = proto::sycl::MallocDeviceResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.ptr.kind(), KIND_SYCL_DEVICE_PTR);
        let state = backend.ptrs.get(decoded.ptr).expect("ptr state present");
        assert_eq!(state.queue.raw(), queue.raw());
        assert_eq!(state.size, 1024);
    }

    #[test]
    fn malloc_with_bogus_queue_errors() {
        let mut backend = SyclBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_SYCL_QUEUE, 0, 999);
        let err = backend
            .handle(proto::sycl_op::MALLOC_DEVICE, 1, &malloc_body(bogus, 1024))
            .expect_err("malloc with bogus queue must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn free_of_live_ptr_succeeds() {
        let mut backend = SyclBackend::new();
        let ptr = alloc_one(&mut backend);

        let resp = backend
            .handle(proto::sycl_op::FREE, 3, &free_body(ptr))
            .expect("free should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The allocation must no longer resolve.
        assert!(backend.ptrs.get(ptr).is_none());
    }

    #[test]
    fn free_of_bogus_ptr_errors() {
        let mut backend = SyclBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_SYCL_DEVICE_PTR, 0, 999);
        let err = backend
            .handle(proto::sycl_op::FREE, 1, &free_body(bogus))
            .expect_err("free with bogus ptr must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn free_of_already_freed_ptr_errors() {
        let mut backend = SyclBackend::new();
        let ptr = alloc_one(&mut backend);

        backend
            .handle(proto::sycl_op::FREE, 3, &free_body(ptr))
            .expect("first free should succeed");

        let err = backend
            .handle(proto::sycl_op::FREE, 4, &free_body(ptr))
            .expect_err("second free must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = SyclBackend::new();
        // Opcode 0 is a core opcode, not in the SYCL namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-sycl opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
