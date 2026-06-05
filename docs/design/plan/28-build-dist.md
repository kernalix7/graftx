# 28. Build, Packaging, Distribution & Configuration

How GraftX will be compiled (Linux `.so` shims + Windows server binary), cross-built in CI, installed and injected into guest applications, run as a Windows service, configured via file/env, and how the two guests will be paired over vsock/ivshmem.

## 28.1 Scope and constraints

This chapter covers everything between `cargo build` and a working remoting session: the build matrix, packaging artifacts, install layout on both guests, shim injection on Linux, the server lifecycle on Windows, the configuration schema, and the pairing handshake setup. It does **not** re-specify the wire format (the Protocol and Serialization chapters (Ch. 06–07)), the transport channel internals (the Transport chapter (Ch. 08)), the client dispatch table (the Client-shim chapter (Ch. 09)), or the server sandbox mechanics (the Server-core chapter (Ch. 10) and the Testing chapter (Ch. 26)) — those are referenced where the build/config touches them.

The dominant constraint is the **hard target split** established in the Workspace chapter (Ch. 05): `graftx-client` only ever produces a Linux `cdylib`, `graftx-server` only ever produces a Windows executable. They share `graftx-protocol` (pure, `#![forbid(unsafe_code)]`) and `graftx-transport`. Packaging must therefore yield two physically separate artifact sets that are *version-locked* to the same protocol revision, because a mismatched pair is a silent corruption bug (versioning in the Versioning chapter (Ch. 29)). A second constraint: the client `.so` is loaded **into untrusted, unmodified guest applications** via `LD_PRELOAD`, so it must be self-contained, must not crash its host process on config errors, and must degrade to a clear diagnostic instead of `panic!`.

## 28.2 Build matrix and profiles

GraftX has two shipping targets plus the host-native build used for protocol/transport unit tests:

| Crate | Target triple | Crate type | Host that builds it |
|-------|---------------|-----------|---------------------|
| `graftx-protocol` | host + both | `rlib` | any |
| `graftx-transport` | host + both | `rlib` | any |
| `graftx-client` | `x86_64-unknown-linux-gnu` | `cdylib`+`rlib` | Linux (native) |
| `graftx-server` | `x86_64-pc-windows-msvc` (primary), `x86_64-pc-windows-gnu` (CI cross) | `bin` | Windows native / Linux cross |

The MSVC triple is the **primary** server target because the GPU vendor SDKs (CUDA, Vulkan loader, D3D interop, AMF) ship MSVC import libraries and the production server links against them. The GNU triple is a *CI smoke target* only: it compiles the non-FFI logic with mingw-w64 on the Linux runner so we catch portability regressions early, but it stubs the native-driver bindings behind a feature flag (`--no-default-features --features stub-drivers`) because the vendor `.lib` files are MSVC-format.

Build profiles will be declared in the root virtual manifest so both shipping crates inherit them:

```toml
# Cargo.toml (workspace root) — release tuned for the remoting hot path
[profile.release]
opt-level = 3
lto = "thin"          # thin LTO: most of fat-LTO's win, far less link time
codegen-units = 1     # client .so is latency-critical on the per-call path
strip = "debuginfo"   # ship small .so / .exe; keep a separate split-debug artifact
# NOTE: `panic` is deliberately NOT set here. The two shipping crates need
# opposite panic strategies (server=abort, client=unwind), and a per-package
# `[profile.release.package.*] panic = ...` override is illegal in Cargo — the
# panic setting is profile-global. The split is achieved by building each crate
# in its OWN cargo invocation with its own RUSTFLAGS, never in one unified build.
```

The `panic` split is the load-bearing decision here (decision D11). The server is built with `panic = "abort"` so a malformed-stream-triggered panic crashes the sandboxed server cleanly (the supervisor restarts it; supervisor/fault model in the Server-core chapter (Ch. 10) and the Security chapter (Ch. 23)) rather than unwinding through a native CUDA/Vulkan call where a Rust unwind across the FFI boundary is undefined behavior. The client **cannot** abort: it lives inside someone else's process, so it must unwind and convert the panic into a returned GL/VK error code at the FFI seam (`catch_unwind` wrapper in the Client-shim chapter (Ch. 09)). Cargo does **not** permit a per-package `panic` override within a single profile — `panic` is a profile-global setting — so the split is realised by building the two crates in **separate cargo invocations**: `cargo build -p graftx-client --release` with `RUSTFLAGS="-C panic=unwind"` (cdylib), and `cargo build -p graftx-server --release` with `RUSTFLAGS="-C panic=abort"` (server bin). The unwinding server therefore uses `parking_lot` mutexes so there is no lock poisoning on unwind. The CI matrix already runs the two as separate jobs (different hosts), so this falls out naturally.

A `release-dbg` profile (`inherits = "release"`, `strip = false`, `debug = 1`) will be provided for capturing symbolicated traces from field bug reports without shipping fat binaries by default.

## 28.3 Cross-compilation and CI

The CI fan-out, keyed off the existing `.github/` workflows, is three independent jobs sharing a build cache:

```text
job: lint        (ubuntu)  fmt --check ; clippy --workspace --all-targets -D warnings
                            clippy must run for BOTH client(linux) and server(win) cfgs
job: client      (ubuntu)  build -p graftx-client --release  (native gnu)
                            test  -p graftx-protocol -p graftx-transport
                            package -> graftx-client-<ver>-linux-x86_64.tar.zst
job: server-win  (windows) build -p graftx-server --release  (msvc)
                            package -> graftx-server-<ver>-win-x86_64.zip
job: server-cross(ubuntu)  build -p graftx-server --target x86_64-pc-windows-gnu
                            --no-default-features --features stub-drivers   (smoke only)
```

`clippy -D warnings` (a project hard rule) must be evaluated under *both* platform `cfg`s, because `#[cfg(target_os = "windows")]` code in the server and `#[cfg(target_os = "linux")]` code in the client is invisible to a single-host clippy run. The lint job therefore runs clippy twice with `--target x86_64-unknown-linux-gnu` and `--target x86_64-pc-windows-gnu` so that conditionally-compiled blocks are actually type-checked. Cross-target clippy needs the std component for the target: `rustup target add x86_64-pc-windows-gnu`.

Cross-linking the server on Linux uses the mingw-w64 linker; `cargo-zigbuild` is the proposed fallback if mingw symbol-resolution proves brittle, since `zig cc` bundles a consistent Windows libc. The `.cargo/config.toml` will pin the linker per target:

```toml
# .cargo/config.toml
[target.x86_64-pc-windows-gnu]
linker = "x86_64-w64-mingw32-gcc"

[target.x86_64-unknown-linux-gnu]
# default cc; -fuse-ld=lld optionally for faster client link
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
```

Reproducibility: CI will pass `--locked` so `Cargo.lock` (already committed) is authoritative, and will record the toolchain hash from `rust-toolchain.toml`. Artifacts are content-addressed (`sha256sums.txt` alongside each archive) so an operator can verify the client `.so` and server `.exe` came from the same commit. A `--version` on the server and a `GRAFTX_VERSION` symbol in the client both embed `env!("CARGO_PKG_VERSION")` plus the protocol revision constant from `graftx-protocol::version`, giving a fast field check that a deployed pair is compatible.

## 28.4 Artifact layout

Two archives, one per guest. They are intentionally separate downloads so an operator cannot accidentally ship the Linux `.so` to Windows or vice versa.

```text
graftx-client-<ver>-linux-x86_64.tar.zst
├── lib/
│   └── libgraftx_client.so          # the cdylib (SONAME libgraftx_client.so.0)
├── bin/
│   └── graftx-run                    # launcher wrapper (sets LD_PRELOAD + env)
├── etc/
│   └── graftx/client.toml.example
├── share/doc/graftx/                 # README, SECURITY notes
└── INSTALL.txt

graftx-server-<ver>-win-x86_64.zip
├── graftx-server.exe
├── graftx-server.toml.example
├── install-service.ps1               # registers Windows service (28.7)
├── uninstall-service.ps1
└── README.txt
```

The cdylib's SONAME is fixed at `libgraftx_client.so.0` (the `0` tracks the protocol-major, not the package version) so the dynamic linker resolves the exported GL/EGL/VK symbols. The `cdylib` link section will set this via `build.rs`:

```rust
// crates/graftx-client/build.rs
fn main() {
    // SONAME so LD_PRELOAD'd symbol interposition is stable across patch releases.
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libgraftx_client.so.0");
    // Keep our exported C symbols; let the linker version-script gate everything else.
    println!("cargo:rustc-cdylib-link-arg=-Wl,--version-script={}/exports.map",
             std::env::var("CARGO_MANIFEST_DIR").unwrap());
}
```

The `exports.map` version script is critical for interposition correctness: it makes *only* the intercepted C entry points (`glClear`, `vkCreateInstance`, `cuInit`, `eglGetProcAddress`, …) globally visible and hides everything else, so the `.so` cannot accidentally interpose unrelated symbols in the host process and so symbol-stripping does not remove the entry points the loader must find.

## 28.5 Linux install and shim injection

The client is delivered as a `cdylib` that interposes the GPU API symbols of an unmodified guest application. Three injection mechanisms are planned, in increasing order of invasiveness:

1. **`LD_PRELOAD` (default).** `graftx-run <app>` sets `LD_PRELOAD=$PREFIX/lib/libgraftx_client.so` then `exec`s the app. The dynamic linker resolves the application's calls to `glClear`/`vkCreateInstance`/etc. against our `.so` first. This is the cleanest path and requires no system change.

2. **ICD/loader manifest hijack (Vulkan/OpenCL/EGL).** For loader-mediated APIs the cleaner path is to register GraftX as the *driver* rather than preload over the loader. The launcher will write a Vulkan ICD JSON pointing `library_path` at our `.so` and export `VK_ICD_FILENAMES`/`VK_DRIVER_FILES`; equivalently `OCL_ICD_VENDORS` for OpenCL and `__EGL_VENDOR_LIBRARY_FILENAMES` for EGL. This makes us a first-class driver the loader dlopen's, which is more robust than symbol preloading for these specific APIs (the loader has a defined ABI we implement; the Client-shim chapter (Ch. 09)).

3. **System-wide `/etc/ld.so.preload`.** Discouraged — it injects into *every* process and is a foot-gun if the `.so` ever faults during init. Documented for headless/container scenarios only, gated behind an explicit `--system` flag.

The launcher wrapper is intentionally tiny and robust:

```text
graftx-run [--config PATH] [--icd vulkan|opencl|egl|all] [--] <app> [args...]
  1. resolve config (28.8) -> validate -> export GRAFTX_* env
  2. select injection mode (LD_PRELOAD default; --icd writes loader manifests to a tmpdir)
  3. probe transport reachability (vsock CID + ivshmem device) — warn early, do not block
  4. exec the target app with the prepared environment
```

**Failure isolation inside the host process.** Because the `.so` runs in the application, its initialization (`#[ctor]`-style constructor that reads config, opens the transport) must never abort the host. The constructor returns a *degraded* state on failure and every interposed entry point checks it:

```rust
// crates/graftx-client/src/init.rs  (sketch)
static STATE: OnceLock<Result<Session, InitError>> = OnceLock::new();

/// Called from each interposed symbol before remoting. Never panics.
fn session() -> Result<&'static Session, GraftxStatus> {
    match STATE.get_or_init(connect_from_env) {
        Ok(s) => Ok(s),
        Err(e) => {
            log_once(e);            // single rate-limited stderr line
            Err(GraftxStatus::Unavailable) // entry point returns API-native error code
        }
    }
}
```

On `Unavailable`, a Vulkan entry returns `VK_ERROR_INITIALIZATION_FAILED`, a CUDA entry returns `CUDA_ERROR_NOT_INITIALIZED`, etc. (status mapping in the Client-shim chapter (Ch. 09)), so the host application sees a normal API error rather than a crash. An optional `GRAFTX_FALLBACK=passthrough` mode will `dlopen` the real driver and forward, but that requires a software/native GPU on the Linux guest and is out of scope for the passthrough-only baseline.

Install is relocatable (`$PREFIX` default `/usr/local`, overridable), with no setuid bits and no daemon — the Linux side is purely a library plus a launcher.

## 28.6 Server install on Windows

The server is a single self-contained `graftx-server.exe`. Its FFI dependencies (CUDA runtime, Vulkan loader, etc.) are *not* bundled — they are the host's installed GPU drivers, which is correct because the whole point is that the Windows guest owns the physical GPU via passthrough. Install steps:

```text
1. unzip graftx-server-<ver>-win-x86_64.zip into  C:\Program Files\GraftX\
2. copy graftx-server.toml.example -> %ProgramData%\GraftX\server.toml ; edit
3. (optional) run install-service.ps1 (Admin) to register the service (28.7)
4. confirm the ivshmem PCI device + virtio-vsock are present (Device Manager / drivers)
```

The server validates at startup that (a) the configured GPU adapter exists, (b) the ivshmem BAR is mappable, and (c) the vsock CID is bindable. Any failure aborts with a numbered diagnostic before it ever touches the untrusted stream.

## 28.7 Windows service lifecycle

For headless operation the server runs as a Windows service. The `windows-service` crate provides the SCM glue; `graftx-server.exe` will detect whether it was launched by the Service Control Manager (no console, specific entry) versus a console and branch accordingly:

```rust
// crates/graftx-server/src/main.rs  (sketch, windows-only)
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    match run_mode() {
        RunMode::Service => service::dispatch("GraftX"),   // SCM control loop
        RunMode::Console => server::run(Config::load()?),  // foreground, Ctrl-C aware
    }
}
```

The service control handler maps SCM signals to the supervisor (supervisor/fault model in the Server-core chapter (Ch. 10) and the Security chapter (Ch. 23)): `Stop`/`Shutdown` drain in-flight sessions then exit; `Pause`/`Continue` quiesce the accept loop without dropping mapped shared memory. The service is registered with a **dedicated low-privilege account**, not `LocalSystem`, consistent with the "sandbox server" requirement (the Security chapter (Ch. 23)) — it gets exactly the GPU-device and ivshmem access it needs and nothing else. `install-service.ps1` will:

```powershell
# install-service.ps1 (essentials)
sc.exe create GraftX binPath= "`"$exe`" --service" start= demand obj= "$svcAccount"
sc.exe failure GraftX reset= 60 actions= restart/5000/restart/5000/restart/5000
sc.exe description GraftX "GraftX GPU API remoting server"
```

`failure` actions delegate crash-restart to the SCM (5 s backoff), complementing the in-process supervisor: a `panic = "abort"` exit (28.2) is treated by SCM as a failure and triggers restart. Logging goes to the Windows Event Log when running as a service and to stderr in console mode; the config selects verbosity (28.8).

## 28.8 Configuration: file and environment

Configuration is layered, lowest-to-highest precedence: **built-in defaults → config file → environment variables → command-line flags**. The schema is shared in `graftx-protocol`-adjacent config types so client and server agree on names. The format is TOML, parsed with `serde` + `toml`, validated into a typed struct (no `unwrap`; errors via `thiserror`).

```rust
// shared config shape (sketch)
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]      // typo in a key is an error, not a silent default
pub struct Transport {
    pub mode: TransportMode,        // Vsock | Ivshmem | Auto
    pub peer_cid: u32,              // vsock CID of the *other* guest
    pub port: u32,                  // vsock port
    pub ivshmem_path: Option<String>, // e.g. "/dev/uio0" (linux) / device id (win)
    pub shm_bytes: u64,             // bulk-plane size; must match both ends
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerLimits {          // resource quotas / backpressure (security)
    pub max_sessions: u32,
    pub max_inflight_bytes: u64,
    pub max_handles_per_session: u32,
    pub copy_in_budget_bytes: u64, // cap on validated bytes copied to server-private mem
}
```

`deny_unknown_fields` is deliberate: a misspelled `peer_cid` silently falling back to a default would point the transport at the wrong guest, so config typos must fail loudly. Every environment variable mirrors a key with a `GRAFTX_` prefix and `__` for nesting (`GRAFTX_TRANSPORT__PEER_CID=3`), resolved by `figment`/manual merge. Selected keys:

| Key (TOML) | Env | Side | Meaning |
|------------|-----|------|---------|
| `transport.mode` | `GRAFTX_TRANSPORT__MODE` | both | vsock / ivshmem / auto |
| `transport.peer_cid` | `GRAFTX_TRANSPORT__PEER_CID` | both | peer guest CID (pairing) |
| `transport.shm_bytes` | `GRAFTX_TRANSPORT__SHM_BYTES` | both | bulk-plane size (must match) |
| `server.limits.max_sessions` | `GRAFTX_SERVER__LIMITS__MAX_SESSIONS` | server | quota |
| `log.level` | `GRAFTX_LOG__LEVEL` | both | error..trace |
| `log.sink` | `GRAFTX_LOG__SINK` | both | stderr / eventlog / file |

The client reads `client.toml` (search order: `--config`, `$GRAFTX_CONFIG`, `$XDG_CONFIG_HOME/graftx/client.toml`, `/etc/graftx/client.toml`); the server reads `%ProgramData%\GraftX\server.toml`. A `graftx-server --check-config` and `graftx-run --check-config` dry-run path validates and prints the *effective* merged config (with secrets redacted) without starting anything — the canonical way to debug a non-connecting pair.

## 28.9 Transport pairing setup

Pairing wires the two guests together; it is **reachability scoping, not authentication** (per project security model — real per-session auth/integrity is planned, the Security chapter (Ch. 23)). Two things must be agreed out-of-band by the operator/hypervisor config:

1. **vsock control plane.** Each guest has a context ID (CID) assigned by the hypervisor (`-device vhost-vsock-pci,guest-cid=N` in QEMU). The Linux client sets `transport.peer_cid` to the Windows server's CID and a fixed `port`; the server binds `(VMADDR_CID_ANY, port)` and accepts only from its configured `peer_cid`. Because both guests are on the same host, the "paired-guest channel" simply means the server rejects connections whose source CID is not the configured peer.

2. **ivshmem bulk plane.** The hypervisor exposes a shared-memory PCI device (`ivshmem-plain` or `ivshmem-doorbell`) to *both* guests backed by the same host memory object. `transport.ivshmem_path` (Linux UIO/vfio node) and the Windows device identify the BAR; `transport.shm_bytes` must be identical on both ends or the ring/region math diverges. The server treats this region as untrusted and copies validated data into server-private memory before use (write-revocation is a hypervisor/ivshmem-device concern, not enforceable in-process).

The setup/verification flow an operator follows:

```text
HYPERVISOR (host):  assign CIDs (linux=4, windows=5); attach ivshmem-plain (size=256M) to both
WINDOWS guest:      server.toml { mode=auto, peer_cid=4, port=9000, shm_bytes=256M }
                    install-service.ps1 ; start service ; --check-config OK
LINUX guest:        client.toml { mode=auto, peer_cid=5, port=9000, shm_bytes=256M }
                    graftx-run --check-config   -> prints effective config, probes reachability
                    graftx-run -- glxgears       -> first real session
HANDSHAKE:          vsock connect -> Hello/Welcome exchange (Protocol chapter Ch.06) -> ivshmem region attach
                    -> ring init -> ready. shm_bytes / protocol-rev mismatch = hard reject here.
```

`mode = auto` means: establish the vsock control plane first (it always works guest-to-guest), exchange capabilities, and use ivshmem for bulk only if both ends advertise the same region size and it maps successfully; otherwise fall back to vsock-only bulk (slower, but functional). This keeps a session usable when ivshmem is misconfigured while still preferring the fast path. The version/size negotiation happening *inside the handshake* (not just from static config) is what guarantees a mismatched client/server pair from 28.3 fails immediately and visibly rather than corrupting the stream — closing the loop between build-time version embedding and runtime pairing.
