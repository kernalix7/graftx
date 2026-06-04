# 비교

[English](COMPARISON.md) | **한국어**

GraftX가 기존 GPU 가상화 및 리모팅 프로젝트와 어떤 관계에 있는지 설명합니다. GraftX의 차별화된 위치는 다음과 같습니다: **guest-to-guest**, **멀티 API(compute 포함)**, 픽셀이나 프레임버퍼가 아니라 **API 호출**을 포워딩합니다.

## 선행 기술

| 프로젝트 | 하는 일 | GraftX와의 차이점 |
| --- | --- | --- |
| **virtio-GPU + Venus / VirGL** | Host → guest OpenGL/Vulkan 반가상화(paravirtualization) | Guest → guest, compute(CUDA/OpenCL/ROCm)를 포함한 멀티 API |
| **Looking Glass** | Windows guest에서 host로의 저지연 프레임버퍼 릴레이 | 프레임버퍼만이 아니라 API 호출을 포워딩 |
| **Sunshine / Moonlight** | 인코딩된 게임 영상 스트리밍 | 픽셀이 아니라 API 호출을 리모팅 |
| **rCUDA / cricket** | 네트워크를 통한 CUDA 리모팅 | 멀티 API, 로컬 guest-to-guest transport |
| **DXVK / VKD3D** | 한 머신 내부에서 Direct3D → Vulkan 변환 | 프로세스 내 API 변환이 아니라, 변형되지 않은 GPU API의 cross-guest transport |

## guest-to-guest를 택한 이유

일반적인 passthrough 구성은 물리 GPU를 정확히 하나의 VM에만 할당합니다. 보통은 벤더 드라이버(NVIDIA/AMD/Intel)와 도구가 가장 완성도 높은 Windows guest가 그 대상입니다. host의 나머지 모든 것은 가속을 잃게 됩니다.

GraftX는 "Windows 화면을 host로 릴레이하는" 도구(Looking Glass)나 "인코딩된 영상을 스트리밍하는" 도구(Sunshine)와는 반대되는 접근을 취합니다. **픽셀**을 옮기는 대신, **API 호출 자체**를 Linux guest에서 Windows guest로 옮기고 결과만 되돌려 받습니다. 이렇게 하면 Linux 애플리케이션을 변형하지 않은 채로 두면서, 그래픽뿐 아니라 compute API(CUDA, OpenCL, ROCm)까지 동작하게 됩니다.

## 트레이드오프

- **Linux guest로의 passthrough 대비:** GraftX는 리모팅 오버헤드가 추가되지만, 두 번째 GPU가 필요 없고 카드를 재할당하기 위한 재부팅도 필요 없습니다.
- **프레임버퍼/영상 릴레이 대비:** GraftX는 compute와 변형되지 않은 API를 지원하지만, API별 포워딩 표면(surface)을 구현하고 유지보수해야 합니다.
- **네트워크 GPU 리모팅(rCUDA) 대비:** GraftX는 로컬 guest-to-guest(공유 메모리 / virtio-vsock)이므로 지연 시간이 훨씬 낮지만, 클러스터가 아니라 같은 위치에 배치된 VM으로 범위가 한정됩니다.
