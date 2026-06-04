# 서드파티 라이선스

[English](../THIRD_PARTY_LICENSES.md) | **한국어**

GraftX는 MIT 라이선스를 따릅니다([LICENSE](../LICENSE) 참고). 이 문서는 빌드/런타임
의존성으로 포함되는 서드파티 구성 요소와 그 상위(upstream) 라이선스를 정리한 것입니다.
GraftX는 소스 트리에 어떠한 서드파티 바이너리도 재배포하지 **않습니다**. 아래의 모든 항목은
빌드 시점에 Cargo가 [crates.io](https://crates.io)에서 가져옵니다.

기계 생성된 전체 저작자 표시(attribution) 세트는
[`cargo-about`](https://github.com/EmbarkStudios/cargo-about) 또는
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny)로 언제든지 다시 생성할 수 있습니다.

```bash
cargo install cargo-about
cargo about generate about.hbs > THIRD_PARTY_LICENSES.generated.html
```

## 현재 의존성

현재 모든 의존성은 관대한(permissive) 라이선스이며 MIT와 호환됩니다. 그중 하나
(`unicode-ident`)는 내장된 유니코드 테이블에 대해 관대한 `Unicode-3.0` 라이선스를
추가로 적용하며, 이는 재배포 시 유니코드 저작자 표시(attribution) 고지를
유지할 것을 요구합니다.

| Crate | Version | License | Upstream |
|---|---|---|---|
| `thiserror` | 2.x | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| `thiserror-impl` | 2.x | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| `proc-macro2` | 1.x | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| `quote` | 1.x | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| `syn` | 2.x | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| `unicode-ident` | 1.x | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |

## 예정된 의존성

GPU API 백엔드가 추가되면 GraftX는 추가 crate를 링크하게 됩니다. 해당 라이선스는 채택
전에 MIT/Apache-2.0 호환성을 검증한 뒤 여기에 추가됩니다.

| Crate | Role | Expected license |
|---|---|---|
| `ash` | Vulkan 바인딩 | MIT OR Apache-2.0 |
| `glow` | OpenGL / GLES 바인딩 | MIT OR Apache-2.0 OR Zlib |
| `opencl3` | OpenCL 바인딩 | Apache-2.0 |
| `cudarc` / `cust` | CUDA 바인딩 | MIT OR Apache-2.0 |
| `wgpu` | WebGPU | MIT OR Apache-2.0 |
| `vsock` | virtio-vsock transport | Apache-2.0 |

> 플랫폼 GPU 드라이버 자체(NVIDIA, AMD, Intel, Mesa)는 번들링되거나 재배포되지
> **않습니다**. GraftX는 Windows 게스트에 설치되어 있는 드라이버를 그대로 호출합니다.
