//! OpenGL backend dispatch.
//!
//! The [`Session`](crate::Session) routes any OpenGL-namespace opcode to this
//! backend. Like the Vulkan backend, the OpenGL backend is a pure-Rust **stub**:
//! it tracks object lifetimes in generational handle tables but performs no real
//! driver work. No OpenGL driver, GPU, or FFI dependency is involved.

use crate::backend::Backend;
use graftx_handles::HandleTable;
use graftx_protocol as proto;

/// Server object kind for a `GlContext` handle.
const KIND_GL_CONTEXT: u8 = 10;
/// Server object kind for a `GlBuffer` handle.
const KIND_GL_BUFFER: u8 = 11;

/// Server-side state tracked for one created OpenGL context.
#[derive(Debug, Default)]
struct CtxState {
    /// Marks the context as live; reserved for future per-context state.
    created: bool,
}

/// Server-side state tracked for one generated OpenGL buffer.
#[derive(Debug)]
struct BufState {
    /// The context handle this buffer was generated in. Recorded for the
    /// lifetime/ownership checks added in a later milestone; read only by tests
    /// for now.
    #[allow(dead_code)]
    context: proto::Handle,
}

/// Pure-Rust OpenGL backend stub.
///
/// Owns generational handle tables for the OpenGL objects it tracks. It answers
/// [`gl_op::CREATE_CONTEXT`](proto::gl_op::CREATE_CONTEXT) by minting a context
/// handle, [`gl_op::MAKE_CURRENT`](proto::gl_op::MAKE_CURRENT) by validating a
/// known context and acknowledging with an empty body, and
/// [`gl_op::GEN_BUFFER`](proto::gl_op::GEN_BUFFER) by minting a buffer handle
/// parented to a known context. The real driver bridge lands in a later
/// milestone; every other OpenGL call is reported as not-yet-implemented via
/// [`ProtocolError::UnknownOpcode`](proto::ProtocolError::UnknownOpcode).
#[derive(Default)]
pub struct GlBackend {
    contexts: HandleTable<CtxState>,
    buffers: HandleTable<BufState>,
}

impl GlBackend {
    /// Create a new OpenGL backend stub with empty object tables.
    #[must_use]
    pub fn new() -> Self {
        Self {
            contexts: HandleTable::new(),
            buffers: HandleTable::new(),
        }
    }
}

impl Backend for GlBackend {
    fn api(&self) -> proto::ApiId {
        proto::ApiId::OpenGl
    }

    fn handle(
        &mut self,
        opcode: u32,
        _req_id: u32,
        body: &[u8],
    ) -> Result<Vec<u8>, proto::ProtocolError> {
        // Reject anything outside the OpenGL namespace outright.
        if proto::opcode_api(opcode) != proto::ApiId::OpenGl as u8 {
            return Err(proto::ProtocolError::UnknownOpcode(opcode));
        }

        match opcode {
            proto::gl_op::CREATE_CONTEXT => {
                // The request body is empty.
                let context = self
                    .contexts
                    .insert(KIND_GL_CONTEXT, CtxState { created: true });
                let mut out = Vec::new();
                proto::gl::CreateContextResponse { context }.encode(&mut out);
                Ok(out)
            }
            proto::gl_op::MAKE_CURRENT => {
                let req = proto::gl::MakeCurrentRequest::decode(body)?;
                // The context must have been created and still be live here.
                let live = self
                    .contexts
                    .get(req.context)
                    .is_some_and(|state| state.created);
                if !live {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                // Acknowledge with an empty body.
                Ok(Vec::new())
            }
            proto::gl_op::GEN_BUFFER => {
                let req = proto::gl::GenBufferRequest::decode(body)?;
                // The context must have been created on this backend.
                let live = self
                    .contexts
                    .get(req.context)
                    .is_some_and(|state| state.created);
                if !live {
                    return Err(proto::ProtocolError::UnknownOpcode(opcode));
                }
                let buffer = self.buffers.insert(
                    KIND_GL_BUFFER,
                    BufState {
                        context: req.context,
                    },
                );
                let mut out = Vec::new();
                proto::gl::GenBufferResponse { buffer }.encode(&mut out);
                Ok(out)
            }
            // Everything else in the OpenGL namespace is not implemented yet.
            other => Err(proto::ProtocolError::UnknownOpcode(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `MAKE_CURRENT` request body for `context`.
    fn make_current_body(context: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::gl::MakeCurrentRequest { context }.encode(&mut body);
        body
    }

    /// Encode a `GEN_BUFFER` request body for `context`.
    fn gen_buffer_body(context: proto::Handle) -> Vec<u8> {
        let mut body = Vec::new();
        proto::gl::GenBufferRequest { context }.encode(&mut body);
        body
    }

    /// Drive a backend through `CREATE_CONTEXT`, returning the minted handle.
    fn create_context(backend: &mut GlBackend) -> proto::Handle {
        let resp = backend
            .handle(proto::gl_op::CREATE_CONTEXT, 1, &[])
            .expect("create context should succeed");
        proto::gl::CreateContextResponse::decode(&resp)
            .expect("decode create context response")
            .context
    }

    #[test]
    fn create_context_returns_kind_ten() {
        let mut backend = GlBackend::new();
        let resp = backend
            .handle(proto::gl_op::CREATE_CONTEXT, 1, &[])
            .expect("create context should succeed");

        let decoded = proto::gl::CreateContextResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.context.kind(), KIND_GL_CONTEXT);
        // The returned handle must resolve in the context table.
        assert!(backend.contexts.get(decoded.context).is_some());
    }

    #[test]
    fn make_current_after_create_returns_empty_ack() {
        let mut backend = GlBackend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(proto::gl_op::MAKE_CURRENT, 2, &make_current_body(context))
            .expect("make current should succeed");
        assert!(resp.is_empty());
    }

    #[test]
    fn gen_buffer_after_create_returns_kind_eleven() {
        let mut backend = GlBackend::new();
        let context = create_context(&mut backend);

        let resp = backend
            .handle(proto::gl_op::GEN_BUFFER, 3, &gen_buffer_body(context))
            .expect("gen buffer should succeed");

        let decoded = proto::gl::GenBufferResponse::decode(&resp).expect("decode response");
        assert_eq!(decoded.buffer.kind(), KIND_GL_BUFFER);
        let state = backend
            .buffers
            .get(decoded.buffer)
            .expect("buffer state present");
        assert_eq!(state.context.raw(), context.raw());
    }

    #[test]
    fn make_current_with_bogus_context_errors() {
        let mut backend = GlBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_GL_CONTEXT, 0, 999);
        let err = backend
            .handle(proto::gl_op::MAKE_CURRENT, 1, &make_current_body(bogus))
            .expect_err("make current with bogus context must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn gen_buffer_with_bogus_context_errors() {
        let mut backend = GlBackend::new();
        // A handle that was never issued by this backend.
        let bogus = proto::Handle::new(KIND_GL_CONTEXT, 0, 999);
        let err = backend
            .handle(proto::gl_op::GEN_BUFFER, 1, &gen_buffer_body(bogus))
            .expect_err("gen buffer with bogus context must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(_)));
    }

    #[test]
    fn out_of_namespace_opcode_errors() {
        let mut backend = GlBackend::new();
        // Opcode 0 is a core opcode, not in the OpenGL namespace.
        let err = backend
            .handle(0, 1, &[])
            .expect_err("non-opengl opcode must error");
        assert!(matches!(err, proto::ProtocolError::UnknownOpcode(0)));
    }
}
