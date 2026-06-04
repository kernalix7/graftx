# 기능

[English](FEATURES.md) | **한국어**

GraftX는 Linux 게스트의 GPU API 호출을 GPU를 소유한 Windows 게스트로 전달합니다. 이 페이지는 API 지원 매트릭스와 출시 계획을 정리합니다. 별도로 표시되기 전까지 모든 항목은 **계획 단계**입니다 — GraftX는 현재 v0.0.0입니다.

## API 지원 범위

지원 범위는 단계별로 확장됩니다. Tier 1은 근간(NVIDIA + AMD + Intel + 크로스 벤더 compute)이며, Tier 2–3은 범위를 넓힙니다. Vulkan은 척추 역할을 합니다 — 얇고 명시적이며 오버헤드가 가장 낮습니다 — 다른 API는 네이티브로 전달되거나 Vulkan을 통해 깔때기처럼 모일 수 있습니다.

### Tier 1 — 근간

| API | 영역 | 벤더 | 상태 |
| --- | --- | --- | --- |
| **Vulkan** | 그래픽스 / compute | cross — 척추 | 계획 |
| **OpenGL** | 그래픽스 | cross | 계획 |
| **OpenGL ES** | 그래픽스 (임베디드 서브셋) | cross | 계획 |
| **EGL** | 컨텍스트 / 서피스 관리 | cross | 계획 |
| **CUDA** | Compute | NVIDIA | 계획 |
| **OpenCL** | Compute | 크로스 벤더 | 계획 |

### Tier 2 — 범위 확장

| API | 영역 | 벤더 | 상태 |
| --- | --- | --- | --- |
| **HIP** | Compute (ROCm 상위의 이식성 레이어) | AMD | 계획 |
| **Level Zero (oneAPI)** | 저수준 compute | Intel | 계획 |
| **VA-API** | 비디오 디코드 / 인코드 | Intel / AMD | 계획 |
| **VDPAU** | 비디오 디코드 | NVIDIA | 계획 |
| **NVENC / NVDEC** | 비디오 인코드 / 디코드 | NVIDIA | 계획 |
| **Vulkan Video** | 비디오 디코드 / 인코드 | cross | 계획 |

### Tier 3 — 도달 범위

| API | 영역 | 벤더 | 상태 |
| --- | --- | --- | --- |
| **SYCL / oneAPI** | Compute (Level Zero 위에서 동작) | cross | 계획 |
| **OptiX** | 레이 트레이싱 (CUDA 위에서 동작) | NVIDIA | 계획 |
| **AMF** | 비디오 인코드 | AMD | 계획 |
| **WebGPU / wgpu** | 그래픽스 / compute | cross | 계획 |
| **GLX** | X11 GL 컨텍스트 연결 | cross | 계획 |

## 설계 원칙

- **범위 우선.** 핵심 목표는 가능한 한 많은 GPU API와 버전을 지원하는 것입니다.
- **낮은 오버헤드.** 제로 카피 버퍼, 커맨드 배칭, 비동기 제출로 리모팅 비용을 낮춥니다.
- **부하 상황에서의 안정성.** 실제 워크로드에서의 정확성이 마이크로 최적화보다 우선합니다.
- **기본적으로 안전.** 서버는 클라이언트의 커맨드 스트림을 신뢰할 수 없는 것으로 취급합니다 ([SECURITY.ko.md](SECURITY.ko.md) 참고).

## 전달 전략

API별로 선택하는 두 가지 전략이 있습니다:

- **네이티브 전달** — API마다 하나의 shim을 두고, 네이티브 Windows 드라이버에서 리플레이합니다. 범위와 성능이 최대지만 유지 관리해야 할 서버 표면이 더 넓습니다.
- **Vulkan 통과(funnel-through-Vulkan)** — 레거시 GL → Vulkan (Zink), OpenCL → Vulkan (clvk/clspv)으로 변환합니다. 서버 코드는 적지만 일부 변환 비용이 발생합니다.

하이브리드 방식이 예상됩니다: CUDA / Vulkan / compute는 네이티브로 전달하고, 레거시 GL은 깔때기 방식으로 처리합니다. 자세한 내용은 [ARCHITECTURE.ko.md](ARCHITECTURE.ko.md)를 참고하세요.
