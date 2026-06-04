# Security Policy

**English** | [한국어](docs/SECURITY.ko.md)

GraftX is a GPU API-remoting layer. A Linux guest's GPU API calls (OpenGL,
OpenGL ES, EGL, Vulkan, CUDA, OpenCL, ROCm/HIP, Level Zero, video codecs, and
related APIs) are intercepted by client shims, serialized, sent over a fast
guest-to-guest transport (virtio-vsock / ivshmem), and replayed by a server on a
Windows guest that owns the physical GPU via PCI passthrough. By its nature this
project has a significant and deliberate attack surface, so security reports are
taken seriously even at this early stage.

## Supported Versions

GraftX is in early development. It is currently versioned `0.0.0` and is pre-1.0
(`0.0.x`). Nothing is stable yet, the wire format and `PROTOCOL_VERSION` may
change without notice, and there are no backward-compatibility or security
guarantees of any kind.

Only the latest commit on the `main` branch is supported. There are no tagged
releases, no maintained release branches, and no backported fixes. If you report
an issue against anything other than current `main`, you will be asked to
reproduce it against current `main` first.

| Version            | Supported          |
| ------------------ | ------------------ |
| latest `main`      | Yes (best effort)  |
| any tagged release | None exist yet     |
| `0.0.x` (general)  | No guarantees      |

During early development, fixes land on `main` when practical. There is no
committed timeline, no security SLA, and no guarantee that any particular issue
will be fixed. **Do not deploy GraftX in any setting where it can receive a
command stream from an untrusted client.** See the threat model below.

## Reporting a Vulnerability

**Report vulnerabilities privately. Do not open public GitHub issues, pull
requests, or discussions for security problems**, and do not disclose details
publicly until a fix is available or we have agreed on a disclosure timeline.

Email reports to:

- **Kim DaeHyun &lt;kernalix7@kodenet.io&gt;**

Please include as much of the following as you can:

- A clear description of the vulnerability and its impact.
- The affected component(s): `graftx-protocol`, `graftx-transport`,
  `graftx-client`, or `graftx-server`.
- The commit hash (current `main`) you reproduced against.
- The transport in use (virtio-vsock or ivshmem) and the guest/host topology.
- The targeted GPU API(s) and driver/vendor (for example, the Vulkan, CUDA, or
  ROCm/HIP path) on the server side, if relevant.
- Step-by-step reproduction instructions, and a proof-of-concept or a crafted
  command stream / malformed payload if you have one.
- Any logs, backtraces, sanitizer output, or crash dumps.
- Your assessment of severity and any suggested remediation.

### What to expect

- **Acknowledgement:** within 7 days of your report.
- **Assessment:** after acknowledgement we will work to confirm the issue,
  determine its scope and severity, and discuss a remediation and disclosure
  plan with you.
- **Credit:** if you would like to be credited, tell us and we will name you
  when the fix is published; otherwise reports are handled confidentially.

Because this is a pre-1.0 project maintained on a best-effort basis, we cannot
commit to a fixed resolution deadline. We will keep you informed of progress.

## Threat model

The core security property of GraftX is that the **server replays an untrusted
command stream against native GPU drivers.** This is the primary attack surface
and it is large by design:

- **The client is untrusted.** A client shim (or anything impersonating one) can
  send arbitrary, malformed, or adversarial commands over the transport. The
  server must not assume the command stream is well-formed or benign.
- **Native drivers are the blast radius.** Replaying attacker-influenced calls
  exercises vendor GPU drivers (graphics, compute, and video) directly. Driver
  bugs, out-of-bounds buffers, integer overflows in size/offset fields,
  use-after-free, and similar defects become reachable from the command stream.
- **Resource exhaustion.** A malicious or buggy client can attempt to exhaust
  GPU memory, host memory, file descriptors, queues, or transport buffers
  (denial of service).
- **Transport exposure.** The guest-to-guest transport (virtio-vsock /
  ivshmem) is a channel into the server. If it is reachable by an untrusted
  party, that party can drive the server. Note that connection scoping
  (virtio-vsock CID/port pairing, and which guests map the shared ivshmem
  region) is a **reachability** control — it limits *who can reach* the
  endpoint — and is **not authentication**: it does not verify *who* a peer is
  or that a given command stream is authorized. Likewise the `PROTOCOL_VERSION`
  handshake is a **compatibility check, not a security boundary**; it confirms
  the two ends speak the same wire format and must never be treated as
  authenticating or authorizing a client.
- **In-guest threats.** Reachability scoping says nothing about callers that are
  already inside a reachable endpoint. A co-resident or compromised process
  inside the Linux guest — or any process able to map the ivshmem region — can
  submit a command stream to the server just as a legitimate client shim would,
  because nothing today binds a session to a verified identity.

### Intended mitigations (design goals)

These describe the direction of the project, not the current state:

- All commands decoded by `graftx-protocol` should be validated and
  bounds-checked before they reach the server's replay path; `ProtocolError`
  is the channel for rejecting malformed input.
- The `graftx-server` should be sandboxed (least privilege, constrained access
  to the host and the GPU) so that a driver compromise is contained.
- The transport should be restricted to explicitly paired guests, never exposed
  to general or untrusted endpoints. Because this is only a reachability
  control, it should be paired with **per-session authentication and message
  integrity** so the server can verify that a peer is an authorized client and
  that the command stream has not been forged or tampered with — not merely that
  the peer was able to reach the endpoint or map the shared region.
- **Resource limits, quotas, and backpressure** should bound what any single
  session can consume: per-session GPU-memory and host-memory caps, ceilings on
  handles, queues, and file descriptors, transport-buffer backpressure so a fast
  or hostile producer cannot overrun the server, and frame-size limits on
  individual messages. These bound the resource-exhaustion threat above.
- On the shared-memory (ivshmem) **bulk data path**, the server must **validate
  a private copy**: the length, offset, and handle fields — and any
  security-relevant buffer the validation depends on — are copied into
  **server-private memory** and validated there, and *only* that private copy is
  handed to the driver. The region stays mapped writable on the client side, so
  validating in place is **unsafe** — it leaves a **double-fetch / TOCTOU**
  window in which the client could change a byte after the check but before use.
  See `docs/ARCHITECTURE.md` ("Security / threat model", *Validate a private copy
  on the ivshmem path*) for the detailed mechanism.
- FFI and `unsafe` code is isolated in dedicated modules with `// SAFETY:`
  rationale; library paths avoid `unwrap()`/`expect()` so untrusted input does
  not trivially crash the process.

### Current hardening status

**GraftX is NOT yet hardened.** Input validation, server sandboxing, transport
pairing/authentication, and resource limits are incomplete or absent. Treat the
entire system as exploitable by any party that can reach the transport.

> Do not expose GraftX to untrusted clients. Run it only between guests you
> fully control, on transports that are not reachable by untrusted parties.
> Until the mitigations above are implemented and reviewed, assume that anyone
> who can submit a command stream can compromise the server and, through it, the
> native GPU drivers.
