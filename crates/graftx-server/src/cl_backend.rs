//! OpenCL backend dispatch.
//!
//! The [`Session`](crate::Session) routes any OpenCL-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, CUDA, and HIP backends, the OpenCL backend
//! is a pure-Rust **stub**: it tracks object lifetimes in generational handle
//! tables but performs no real driver work. No OpenCL SDK, GPU, or FFI
//! dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for an OpenCL context handle.
const KIND_CL_CONTEXT: u8 = 40;
/// Server object kind for an OpenCL memory-buffer handle.
const KIND_CL_MEM: u8 = 41;

/// Server-side state tracked for one created OpenCL context.
#[derive(Debug, Default)]
struct ClCtxState {
    /// Marks the context as live; reserved for future per-context state.
    #[allow(dead_code)]
    created: bool,
}

/// Server-side state tracked for one OpenCL memory buffer.
#[derive(Debug)]
struct ClBufState {
    /// The context handle this buffer was created in. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    context: proto::Handle,
    /// Size of the buffer in bytes.
    #[allow(dead_code)]
    size: u64,
}

/// Pure-Rust OpenCL backend stub.
///
/// Owns generational handle tables for the OpenCL objects it tracks. It answers
/// [`cl_op::CREATE_CONTEXT`](proto::cl_op::CREATE_CONTEXT) by minting a context
/// handle, [`cl_op::CREATE_BUFFER`](proto::cl_op::CREATE_BUFFER) by minting a
/// memory-buffer handle parented to a known context, and
/// [`cl_op::RELEASE_BUFFER`](proto::cl_op::RELEASE_BUFFER) by removing a known
/// memory-buffer handle and acknowledging with an empty body. The real driver
/// bridge lands in a later milestone; every other OpenCL call is reported as
/// not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct ClBackend {
    contexts: HandleTable<ClCtxState>,
    buffers: HandleTable<ClBufState>,
}

impl ClBackend {
    /// Create a new OpenCL backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contexts: HandleTable::new(),
            buffers: HandleTable::new(),
        }
    }
}

impl Backend for ClBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::OpenCl
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the OpenCL namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::OpenCl as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::cl_op::CREATE_CONTEXT => {
                // The request body is empty, so there is nothing to decode.
                let context = self
                    .contexts
                    .insert(KIND_CL_CONTEXT, ClCtxState { created: true });
                let mut out = Vec::new();
                proto::cl::CreateContextResponse { context }.encode(&mut out);
                Ok(out)
            }
            proto::cl_op::CREATE_BUFFER => {
                let req = proto::cl::CreateBufferRequest::decode(body)?;
                // The context must have been created on this backend.
                if self.contexts.get(req.context).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let mem = self.buffers.insert(
                    KIND_CL_MEM,
                    ClBufState {
                        context: req.context,
                        size: req.size,
                    },
                );
                let mut out = Vec::new();
                proto::cl::CreateBufferResponse { mem }.encode(&mut out);
                Ok(out)
            }
            proto::cl_op::RELEASE_BUFFER => {
                let req = proto::cl::ReleaseBufferRequest::decode(body)?;
                // The buffer must have been created on this backend and still be
                // live; removing it both validates and releases it.
                if self.buffers.remove(req.mem).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            // Everything else in the OpenCL namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `CREATE_BUFFER` request body for `context` and `size`.
    fn create_buffer_body(context: proto::Handle, size: u64) -> Vec<u8> {
        let mut body = Vec::new();
        proto::cl::CreateBufferRequest { context, size }.encode(&mut body);
        body
    }

    /// Encode a `RELEASE_BUFFER` request body for `mem`.
    fn release_buffer_body(mem: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::cl::ReleaseBufferRequest { mem }.encode(&mut body);
        body
    }

    /// Drive a backend through `CREATE_CONTEXT`, returning the minted context
    /// handle.
    fn create_context(backend: &mut ClBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::cl_op::CREATE_CONTEXT, 1, &[])
            .expect("create context should succeed");
        proto::cl::CreateContextResponse::decode(&resp)
            .expect("decode create context response")
            .context
    }

    /// Drive a backend through `CREATE_CONTEXT` then `CREATE_BUFFER`, returning
    /// the minted memory-buffer handle.
    fn create_buffer_one(backend: &mut ClBackend) -> proto::Handle {
        let context = create_context(backend);
        let resp = backend
            .handle(
                proto::cl_op::CREATE_BUFFER,
                2,
                &create_buffer_body(context, 4096),
            )
            .expect("create buffer should succeed");
        proto::cl::CreateBufferResponse::decode(&resp)
            .expect("decode create buffer response")
            .mem
    }

    #[test]
    fn create_context_returns_kind_forty() {
        let mut backend = ClBackend::new();
        let resp = backend
            .handle(proto::cl_op::CREATE_CONTEXT, 1, &[])
            .expect("create context should succeed");

        let decoded = proto::cl::CreateContextResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.context.kind(), KIND_CL_CONTEXT);
        // The returned handle must resolve in the context table.
        assert!(backend.contexts.get(decoded.context).is_some());
    }

    #[test]
    fn create_buffer_after_context_returns_kind_forty_one() {
        let mut backend = ClBackend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(
                proto::cl_op::CREATE_BUFFER,
                2,
                &create_buffer_body(context, 1024),
            )
            .expect("create buffer should succeed");

        let decoded = proto::cl::CreateBufferResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.mem.kind(), KIND_CL_MEM);
        let state = backend
            .buffers
            .get(decoded.mem)
            .expect("buffer state present");
        assert_eq!(state.context.raw(), context.raw());
        assert_eq!(state.size, 1024);
    }

    #[test]
    fn create_buffer_with_bogus_context_errors() {
        let mut backend = ClBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_CL_CONTEXT, 0, 999);
        let err = backend
            .handle(
                proto::cl_op::CREATE_BUFFER,
                1,
                &create_buffer_body(bogus, 1024),
            )
            .expect_err("create buffer with bogus context must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn release_buffer_of_live_mem_succeeds() {
        let mut backend = ClBackend::new();
        let mem = create_buffer_one(&mut backend);

        let resp = backend
            .handle(proto::cl_op::RELEASE_BUFFER, 3, &release_buffer_body(mem))
            .expect("release buffer should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The buffer must no longer resolve.
        assert!(backend.buffers.get(mem).is_none());
    }

    #[test]
    fn release_buffer_of_bogus_mem_errors() {
        let mut backend = ClBackend::new();
        // A handle that was never created by this backend.
        let bogus = proto::Handle::new(KIND_CL_MEM, 0, 999);
        let err = backend
            .handle(
                proto::cl_op::RELEASE_BUFFER,
                1,
                &release_buffer_body(bogus),
            )
            .expect_err("release buffer with bogus mem must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn release_buffer_of_already_released_mem_errors() {
        let mut backend = ClBackend::new();
        let mem = create_buffer_one(&mut backend);

        backend
            .handle(proto::cl_op::RELEASE_BUFFER, 3, &release_buffer_body(mem))
            .expect("first release should succeed");

        let err = backend
            .handle(proto::cl_op::RELEASE_BUFFER, 4, &release_buffer_body(mem))
            .expect_err("second release must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = ClBackend::new();
        // Opcode 0 is a core opcode, not in the OpenCL namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-opencl opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
