# Third-Party Licenses

**English** | [한국어](docs/THIRD_PARTY_LICENSES.ko.md)

GraftX is MIT-licensed (see [LICENSE](LICENSE)). This document lists the
third-party components pulled in as build/runtime dependencies, together with
their upstream licenses. GraftX does **not** redistribute any third-party
binaries in its source tree; everything below is fetched by Cargo from
[crates.io](https://crates.io) at build time.

The full, machine-generated attribution set can be regenerated at any time with
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) or
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny):

```bash
cargo install cargo-about
cargo about generate about.hbs > THIRD_PARTY_LICENSES.generated.html
```

## Current dependencies

All current dependencies are permissively licensed and MIT-compatible; one
(`unicode-ident`) additionally carries the permissive `Unicode-3.0` license for
its embedded Unicode tables, which requires preserving the Unicode attribution
notice on redistribution.

| Crate | Version | License | Upstream |
|---|---|---|---|
| `thiserror` | 2.x | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| `thiserror-impl` | 2.x | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| `proc-macro2` | 1.x | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| `quote` | 1.x | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| `syn` | 2.x | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| `unicode-ident` | 1.x | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |

## Planned dependencies

As the GPU API backends land, GraftX will link additional crates. Their licenses
will be vetted for MIT/Apache-2.0 compatibility before adoption and added here:

| Crate | Role | Expected license |
|---|---|---|
| `ash` | Vulkan bindings | MIT OR Apache-2.0 |
| `glow` | OpenGL / GLES bindings | MIT OR Apache-2.0 OR Zlib |
| `opencl3` | OpenCL bindings | Apache-2.0 |
| `cudarc` / `cust` | CUDA bindings | MIT OR Apache-2.0 |
| `wgpu` | WebGPU | MIT OR Apache-2.0 |
| `vsock` | virtio-vsock transport | Apache-2.0 |

> The platform GPU drivers themselves (NVIDIA, AMD, Intel, Mesa) are **not**
> bundled or redistributed — GraftX calls into whatever the Windows guest has
> installed.
