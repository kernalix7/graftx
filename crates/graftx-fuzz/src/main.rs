//! Deterministic robustness fuzzer for the GraftX protocol decoders.
//!
//! The wire format is the single source of truth for the control plane, so a
//! decoder that panics on a hostile buffer is a denial-of-service hole: the
//! peer controls every byte. This binary feeds pseudo-random buffers of varied
//! lengths to [`graftx_protocol::decode_frame`] and to every `Body::decode`
//! entrypoint, asserting that each one returns `Ok` or a `ProtocolError` rather
//! than panicking.
//!
//! There is no external fuzzer and no `rand` dependency: a small seeded LCG
//! drives the byte generation so a given seed always exercises the same inputs.
//! That makes a discovered crash reproducible from the seed alone and lets the
//! unit test assert a deterministic input count.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::process::ExitCode;

use graftx_protocol::{
    amf, cl, cuda, decode_frame, gl, hip, l0, optix, sycl, video, vk, wgpu, Hello, Welcome,
};

/// Default number of inputs to exercise when no count is given on the command
/// line.
const DEFAULT_ITERS: usize = 100_000;

/// Largest buffer the generator will produce, in bytes. Kept small: the
/// decoders bottom out on short reads, so longer buffers add cost without
/// reaching new branches.
const MAX_BUF_LEN: usize = 64;

/// A 64-bit linear congruential generator. Deterministic and dependency-free;
/// the constants are the well-known PCG/Numerical-Recipes multiplier and
/// increment, which give a full 2^64 period over the whole state word.
struct Lcg {
    state: u64,
}

impl Lcg {
    /// Seed the generator. Any seed is valid; a fixed seed yields a fixed
    /// stream.
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Advance the state and return the next 64-bit value.
    fn next_u64(&mut self) -> u64 {
        // x_{n+1} = a * x_n + c (mod 2^64); wrapping is the modulus.
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// Return a value in `0..bound`. `bound` must be non-zero.
    fn next_below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Fill `buf` with a fresh pseudo-random buffer of length `0..=MAX_BUF_LEN`
/// drawn from `rng`. Reuses the backing allocation across calls.
fn fill_buf(rng: &mut Lcg, buf: &mut Vec<u8>) {
    let len = rng.next_below(MAX_BUF_LEN + 1);
    buf.clear();
    buf.reserve(len);
    // Emit eight bytes at a time from each LCG draw to keep the byte stream
    // cheap relative to the per-input decoder fan-out.
    while buf.len() < len {
        let word = rng.next_u64().to_le_bytes();
        let take = (len - buf.len()).min(word.len());
        buf.extend_from_slice(&word[..take]);
    }
}

/// Feed a single buffer to every decoder entrypoint. The `let _ =` bindings
/// keep the compiler from eliding the calls; the property under test is simply
/// that none of them panics. Each decoder returns `Result`, so a panic here is
/// a real robustness bug rather than expected error handling.
fn exercise(buf: &[u8]) {
    // Frame header + body split.
    let _ = decode_frame(buf);

    // Handshake bodies.
    let _ = Hello::decode(buf);
    let _ = Welcome::decode(buf);

    // Vulkan bodies.
    let _ = vk::CreateInstanceRequest::decode(buf);
    let _ = vk::CreateInstanceResponse::decode(buf);
    let _ = vk::EnumeratePhysicalDevicesRequest::decode(buf);
    let _ = vk::EnumeratePhysicalDevicesResponse::decode(buf);
    let _ = vk::CreateDeviceRequest::decode(buf);
    let _ = vk::CreateDeviceResponse::decode(buf);
    let _ = vk::GetDeviceQueueRequest::decode(buf);
    let _ = vk::GetDeviceQueueResponse::decode(buf);
    let _ = vk::AllocateMemoryRequest::decode(buf);
    let _ = vk::AllocateMemoryResponse::decode(buf);
    let _ = vk::CreateBufferRequest::decode(buf);
    let _ = vk::CreateBufferResponse::decode(buf);
    let _ = vk::BindBufferMemoryRequest::decode(buf);
    let _ = vk::CreateCommandPoolRequest::decode(buf);
    let _ = vk::CreateCommandPoolResponse::decode(buf);
    let _ = vk::AllocateCommandBufferRequest::decode(buf);
    let _ = vk::AllocateCommandBufferResponse::decode(buf);
    let _ = vk::QueueSubmitRequest::decode(buf);
    let _ = vk::CmdCopyBufferRequest::decode(buf);
    let _ = vk::CmdDrawRequest::decode(buf);
    let _ = vk::DestroyBufferRequest::decode(buf);
    let _ = vk::FreeMemoryRequest::decode(buf);
    let _ = vk::DestroyCommandPoolRequest::decode(buf);

    // OpenGL bodies.
    let _ = gl::CreateContextResponse::decode(buf);
    let _ = gl::MakeCurrentRequest::decode(buf);
    let _ = gl::GenBufferRequest::decode(buf);
    let _ = gl::GenBufferResponse::decode(buf);

    // CUDA bodies.
    let _ = cuda::CtxCreateRequest::decode(buf);
    let _ = cuda::CtxCreateResponse::decode(buf);
    let _ = cuda::MemAllocRequest::decode(buf);
    let _ = cuda::MemAllocResponse::decode(buf);
    let _ = cuda::MemFreeRequest::decode(buf);

    // HIP bodies.
    let _ = hip::MallocRequest::decode(buf);
    let _ = hip::MallocResponse::decode(buf);
    let _ = hip::FreeRequest::decode(buf);
    let _ = hip::StreamCreateResponse::decode(buf);

    // OpenCL bodies.
    let _ = cl::CreateContextResponse::decode(buf);
    let _ = cl::CreateBufferRequest::decode(buf);
    let _ = cl::CreateBufferResponse::decode(buf);
    let _ = cl::ReleaseBufferRequest::decode(buf);

    // Level Zero bodies.
    let _ = l0::ContextCreateResponse::decode(buf);
    let _ = l0::MemAllocDeviceRequest::decode(buf);
    let _ = l0::MemAllocDeviceResponse::decode(buf);
    let _ = l0::MemFreeRequest::decode(buf);

    // Video bodies.
    let _ = video::CreateDecodeSessionRequest::decode(buf);
    let _ = video::CreateDecodeSessionResponse::decode(buf);
    let _ = video::DecodeFrameRequest::decode(buf);
    let _ = video::DecodeFrameResponse::decode(buf);
    let _ = video::DestroySessionRequest::decode(buf);

    // WebGPU bodies.
    let _ = wgpu::RequestDeviceResponse::decode(buf);
    let _ = wgpu::CreateBufferRequest::decode(buf);
    let _ = wgpu::CreateBufferResponse::decode(buf);
    let _ = wgpu::DestroyBufferRequest::decode(buf);

    // OptiX bodies.
    let _ = optix::ContextCreateResponse::decode(buf);
    let _ = optix::PipelineCreateRequest::decode(buf);
    let _ = optix::PipelineCreateResponse::decode(buf);
    let _ = optix::DestroyRequest::decode(buf);

    // SYCL bodies.
    let _ = sycl::QueueCreateResponse::decode(buf);
    let _ = sycl::MallocDeviceRequest::decode(buf);
    let _ = sycl::MallocDeviceResponse::decode(buf);
    let _ = sycl::FreeRequest::decode(buf);

    // AMF bodies.
    let _ = amf::CreateEncoderRequest::decode(buf);
    let _ = amf::CreateEncoderResponse::decode(buf);
    let _ = amf::EncodeFrameRequest::decode(buf);
    let _ = amf::EncodeFrameResponse::decode(buf);
    let _ = amf::DestroyEncoderRequest::decode(buf);
}

/// Run `n` fuzz iterations from `seed`, returning the number of distinct input
/// buffers exercised (equal to `n`). Deterministic in both `seed` and `n`: the
/// same arguments always feed the same buffers, so a crash reproduces and the
/// count is stable.
pub fn fuzz_iter(seed: u64, n: usize) -> usize {
    let mut rng = Lcg::new(seed);
    let mut buf = Vec::with_capacity(MAX_BUF_LEN);
    for _ in 0..n {
        fill_buf(&mut rng, &mut buf);
        exercise(&buf);
    }
    n
}

/// Parse the iteration count from the first CLI argument, falling back to
/// [`DEFAULT_ITERS`] when absent. Returns `None` on an unparsable argument.
fn parse_iters(arg: Option<String>) -> Option<usize> {
    match arg {
        Some(s) => s.parse::<usize>().ok(),
        None => Some(DEFAULT_ITERS),
    }
}

fn main() -> ExitCode {
    // Fixed default seed keeps the binary deterministic across runs; the count
    // comes from argv so the harness can dial the workload up.
    const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

    let mut args = std::env::args().skip(1);
    let iters = match parse_iters(args.next()) {
        Some(n) => n,
        None => {
            eprintln!("usage: graftx-fuzz [iterations]");
            return ExitCode::FAILURE;
        }
    };

    let processed = fuzz_iter(SEED, iters);
    println!("graftx-fuzz: exercised {processed} inputs (seed {SEED:#018x}) with no panic");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzz_runs_without_panic() {
        // A few thousand iterations across every decoder; the test passing at
        // all is the no-panic assertion.
        let processed = fuzz_iter(0xDEAD_BEEF_CAFE_F00D, 4_000);
        assert_eq!(processed, 4_000, "fuzz_iter must report every input it ran");
    }

    #[test]
    fn input_count_is_deterministic() {
        // Same seed and count must always report the same number of inputs.
        let a = fuzz_iter(1, 2_500);
        let b = fuzz_iter(1, 2_500);
        assert_eq!(a, b);
        assert_eq!(a, 2_500);
    }

    #[test]
    fn zero_iterations_processes_nothing() {
        assert_eq!(fuzz_iter(42, 0), 0);
    }

    #[test]
    fn parse_iters_defaults_when_absent() {
        assert_eq!(parse_iters(None), Some(DEFAULT_ITERS));
    }

    #[test]
    fn parse_iters_reads_a_number() {
        assert_eq!(parse_iters(Some("250".to_string())), Some(250));
    }

    #[test]
    fn parse_iters_rejects_garbage() {
        assert_eq!(parse_iters(Some("not-a-number".to_string())), None);
    }
}
