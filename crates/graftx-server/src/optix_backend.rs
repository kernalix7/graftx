//! OptiX backend dispatch.
//!
//! The [`Session`](crate::Session) routes any OptiX-namespace opcode to this
//! backend. Like the Vulkan, OpenGL, CUDA, HIP, OpenCL, and Level Zero
//! backends, the OptiX backend is a pure-Rust **stub**: it tracks object
//! lifetimes in generational handle tables but performs no real driver work. No
//! OptiX SDK, GPU, or FFI dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for an OptiX device-context handle.
const KIND_OPTIX_CONTEXT: u8 = 80;
/// Server object kind for an OptiX ray-tracing pipeline handle.
const KIND_OPTIX_PIPELINE: u8 = 81;

/// Server-side state tracked for one created OptiX device context.
#[derive(Debug, Default)]
struct OptixCtxState {
    /// Marks the context as live; reserved for future per-context state.
    #[allow(dead_code)]
    created: bool,
}

/// Server-side state tracked for one created OptiX ray-tracing pipeline.
#[derive(Debug)]
struct OptixPipelineState {
    /// The context handle this pipeline was created in. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    context: proto::Handle,
}

/// Pure-Rust OptiX backend stub.
///
/// Owns generational handle tables for the OptiX objects it tracks. It answers
/// [`optix_op::CONTEXT_CREATE`](proto::optix_op::CONTEXT_CREATE) by minting a
/// context handle, [`optix_op::PIPELINE_CREATE`](proto::optix_op::PIPELINE_CREATE)
/// by minting a pipeline handle parented to a known context, and
/// [`optix_op::DESTROY`](proto::optix_op::DESTROY) by removing a known context
/// or pipeline handle and acknowledging with an empty body. The real driver
/// bridge lands in a later milestone; every other OptiX call is reported as
/// not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct OptixBackend {
    contexts: HandleTable<OptixCtxState>,
    pipelines: HandleTable<OptixPipelineState>,
}

impl OptixBackend {
    /// Create a new OptiX backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contexts: HandleTable::new(),
            pipelines: HandleTable::new(),
        }
    }
}

impl Backend for OptixBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::OptiX
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the OptiX namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::OptiX as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::optix_op::CONTEXT_CREATE => {
                // The request body is empty, so there is nothing to decode.
                let context = self
                    .contexts
                    .insert(KIND_OPTIX_CONTEXT, OptixCtxState { created: true });
                let mut out = Vec::new();
                proto::optix::ContextCreateResponse { context }.encode(&mut out);
                Ok(out)
            }
            proto::optix_op::PIPELINE_CREATE => {
                let req = proto::optix::PipelineCreateRequest::decode(body)?;
                // The context must have been created on this backend.
                if self.contexts.get(req.context).is_none() {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let pipeline = self.pipelines.insert(
                    KIND_OPTIX_PIPELINE,
                    OptixPipelineState {
                        context: req.context,
                    },
                );
                let mut out = Vec::new();
                proto::optix::PipelineCreateResponse { pipeline }.encode(&mut out);
                Ok(out)
            }
            proto::optix_op::DESTROY => {
                let req = proto::optix::DestroyRequest::decode(body)?;
                // A destroy targets either a context or a pipeline. Try each
                // table in turn; removing the handle both validates and frees
                // it. An unknown handle is an error.
                if self.pipelines.remove(req.handle).is_some()
                    || self.contexts.remove(req.handle).is_some()
                {
                    // Acknowledge with an empty body.
                    Ok(Vec::new())
                } else {
                    Err(proto::ProtocolError::UnknownOpcode(opcode))
                }
            }
            // Everything else in the OptiX namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `PIPELINE_CREATE` request body for `context`.
    fn pipeline_body(context: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::optix::PipelineCreateRequest { context }.encode(&mut body);
        body
    }

    /// Encode a `DESTROY` request body for `handle`.
    fn destroy_body(handle: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::optix::DestroyRequest { handle }.encode(&mut body);
        body
    }

    /// Drive a backend through `CONTEXT_CREATE`, returning the minted context
    /// handle.
    fn create_context(backend: &mut OptixBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::optix_op::CONTEXT_CREATE, 1, &[])
            .expect("context create should succeed");
        proto::optix::ContextCreateResponse::decode(&resp)
            .expect("decode context create response")
            .context
    }

    /// Drive a backend through `CONTEXT_CREATE` then `PIPELINE_CREATE`,
    /// returning the minted pipeline handle.
    fn create_pipeline(backend: &mut OptixBackend) -> proto::Handle {
        let context = create_context(backend);
        let resp = backend
            .handle(proto::optix_op::PIPELINE_CREATE, 2, &pipeline_body(context))
            .expect("pipeline create should succeed");
        proto::optix::PipelineCreateResponse::decode(&resp)
            .expect("decode pipeline create response")
            .pipeline
    }

    #[test]
    fn context_create_returns_kind_eighty() {
        let mut backend = OptixBackend::new();
        let resp = backend
            .handle(proto::optix_op::CONTEXT_CREATE, 1, &[])
            .expect("context create should succeed");

        let decoded = proto::optix::ContextCreateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.context.kind(), KIND_OPTIX_CONTEXT);
        // The returned handle must resolve in the context table.
        assert!(backend.contexts.get(decoded.context).is_some());
    }

    #[test]
    fn pipeline_create_after_context_returns_kind_eighty_one() {
        let mut backend = OptixBackend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(proto::optix_op::PIPELINE_CREATE, 2, &pipeline_body(context))
            .expect("pipeline create should succeed");

        let decoded = proto::optix::PipelineCreateResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.pipeline.kind(), KIND_OPTIX_PIPELINE);
        let state = backend
            .pipelines
            .get(decoded.pipeline)
            .expect("pipeline state present");
        assert_eq!(state.context.raw(), context.raw());
    }

    #[test]
    fn pipeline_create_with_bogus_context_errors() {
        let mut backend = OptixBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_OPTIX_CONTEXT, 0, 999);
        let err = backend
            .handle(proto::optix_op::PIPELINE_CREATE, 1, &pipeline_body(bogus))
            .expect_err("pipeline create with bogus context must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_of_live_context_succeeds() {
        let mut backend = OptixBackend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(proto::optix_op::DESTROY, 2, &destroy_body(context))
            .expect("destroy should succeed");
        // The reply is an empty ack body.
        assert!(resp.is_empty());
        // The context must no longer resolve.
        assert!(backend.contexts.get(context).is_none());
    }

    #[test]
    fn destroy_of_live_pipeline_succeeds() {
        let mut backend = OptixBackend::new();
        let pipeline = create_pipeline(&mut backend);

        let resp = backend
            .handle(proto::optix_op::DESTROY, 3, &destroy_body(pipeline))
            .expect("destroy should succeed");
        assert!(resp.is_empty());
        // The pipeline must no longer resolve.
        assert!(backend.pipelines.get(pipeline).is_none());
    }

    #[test]
    fn destroy_of_bogus_handle_errors() {
        let mut backend = OptixBackend::new();
        // A handle that was never minted by this backend.
        let bogus = proto::Handle::new(KIND_OPTIX_CONTEXT, 0, 999);
        let err = backend
            .handle(proto::optix_op::DESTROY, 1, &destroy_body(bogus))
            .expect_err("destroy of bogus handle must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn destroy_of_already_destroyed_handle_errors() {
        let mut backend = OptixBackend::new();
        let context = create_context(&mut backend);

        backend
            .handle(proto::optix_op::DESTROY, 2, &destroy_body(context))
            .expect("first destroy should succeed");

        let err = backend
            .handle(proto::optix_op::DESTROY, 3, &destroy_body(context))
            .expect_err("second destroy must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = OptixBackend::new();
        // Opcode 0 is a core opcode, not in the OptiX namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-optix opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
