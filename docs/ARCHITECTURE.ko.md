# GraftX 아키텍처

[English](ARCHITECTURE.md) | **한국어**

GraftX가 Linux 게스트의 GPU API 호출을 물리 GPU를 소유한 Windows 게스트로
원격 전달하는 방식에 대한 설계 심층 분석.

> **상태:** 초기 개발 단계(`0.0.0`). 와이어 프로토콜, transport, 내부 구조는
> **안정적이지 않습니다**. 아직 구현되지 않은 동작을 설명하는 절은
> **(계획)** 으로 표시됩니다. 현재 workspace에는 crate 골격, 협상된
> `PROTOCOL_VERSION`, `Transport` trait, 그리고 자신이 구사하는 프로토콜
> 버전을 출력하는 서버가 포함되어 있습니다 — 여기 설명된 그 밖의 모든 것은
> 이 골격이 자라나고 있는 설계입니다.

---

## 개요

단일 물리 GPU는 흔히 PCI passthrough를 통해 **하나의** 가상 머신에
넘겨집니다 — 일반적으로 벤더 드라이버와 도구가 가장 잘 지원되는
**Windows 게스트**입니다. 그러면 같은 호스트의 다른 모든 게스트는 가속
없이 남겨집니다.

GraftX는 그 간극을 메우는 **API 원격(API-remoting) 계층**입니다. Linux 게스트
애플리케이션은 자신의 일반적인 GPU API(OpenGL, OpenGL ES, EGL, Vulkan,
CUDA, OpenCL, ROCm/HIP, Level Zero, video codecs, …)를 전혀 수정하지 않은 채로
계속 호출합니다. GraftX는 이 호출들을 Linux 게스트에서 가로채(intercept)
**직렬화(serialize)** 하고, 빠른 **게스트 간(guest-to-guest) transport**를
통해 전송한 뒤, Windows 게스트에서 native 드라이버에 대해 **재생(replay)**
합니다. 결과와 surface는 Linux 쪽으로 반환됩니다.

이것은 *API 원격(API remoting)* 이며, paravirtualization도 아니고 pixel
streaming도 아닙니다:

- 호스트가 가상화한 GPU 디바이스가 아니라 **API 호출**을 전달합니다
  (virtio-GPU / Venus / VirGL과 다름).
- 완성된 framebuffer가 아니라 **API 호출**을 전달합니다(Looking Glass와 다름)
  — 인코딩된 비디오 스트림도 아닙니다(Sunshine / Moonlight와 다름).
- 네트워크 링크가 아니라 같은 호스트의 두 VM 사이의 **로컬 게스트 간
  (guest-to-guest)** 채널을 대상으로 합니다(rCUDA와 다름).

### 데이터 흐름

```
        LINUX GUEST (untrusted client)                    WINDOWS GUEST (owns the GPU)
  ┌───────────────────────────────────────┐        ┌──────────────────────────────────────┐
  │  application                           │        │  graftx-server (replay engine)         │
  │     │  calls libvulkan.so / libGL.so / │        │     ▲   decode + validate + replay      │
  │     │  libcuda.so / … (unmodified)     │        │     │                                    │
  │     ▼                                  │        │     │                                    │
  │  graftx-client shim (cdylib)           │        │  graftx-protocol (decode)              │
  │     │  intercept C entry point         │        │     ▲                                  │
  │     ▼                                  │        │     │  command frames                  │
  │  graftx-protocol (encode)              │        │     │                                  │
  │     │  command frames                  │        │  graftx-transport ::recv()             │
  │     ▼                                  │        │     ▲                                  │
  │  graftx-transport ::send() ────────────┼──┐     │     │                                  │
  └───────────────────────────────────────┘  │     └─────┼──────────────────────────────────┘
                                              │           │            ▲ native driver
       guest-to-guest transport              │           │            │ (Vulkan / CUDA / …)
       (virtio-vsock | ivshmem) ─────────────┘           │            ▼
                                              ▲           │      ┌──────────────┐
  return path: results / surfaces / status ───┘           │      │ physical GPU │
       graftx-transport ::recv()  ◀──── graftx-transport ::send() │ (passthrough)│
                                                                  └──────────────┘
```

개념적으로:

```
Linux client  ──encode──▶  transport  ──decode──▶  Windows server  ──replay──▶  GPU
      ▲                                                                            │
      └──────────────────────  results / surfaces / status  ◀─────────────────────┘
```

---

## 구성 요소

이 시스템은 `crates/` 아래 네 개 crate로 이루어진 Cargo workspace입니다. 이
분할은 의도적입니다: 프로토콜과 transport는 양쪽에서 공유되는 호스트 중립적
라이브러리이고, 클라이언트와 서버는 링크의 서로 반대쪽 끝입니다.

### `graftx-protocol` (lib)

양쪽 끝이 공유하는 **와이어 포맷(wire format)** 과 인코딩/디코딩 로직을
소유합니다.

- `PROTOCOL_VERSION`(현재 `0`, 사전 안정화 단계 표식 — 포맷이 고정될 때까지
  모든 호환성 깨짐(breaking change)마다 올림)을 정의합니다. 양쪽은 어떤 GPU
  호출이 전달되기 전에 handshake 동안 이를 협상합니다.
- 인코딩 또는 디코딩 중에 발생하는 `ProtocolError`(예: `UnexpectedEof`,
  `UnknownOpcode`)를 정의합니다.
- **(계획)** API별 명령 opcode, 인자 마샬링(argument marshalling), framing,
  handle/object 매핑, 그리고 handshake 메시지 자체.

이 crate는 **순수하고 호스트 중립적이며 FFI도 I/O도 포함하지 않습니다**.
와이어 위의 한 바이트가 무엇을 의미하는지에 대한 단일 진실 공급원(single
source of truth)이며, 클라이언트와 서버는 이를 정확히 일치시켜야 합니다.
unsafe 코드와 플랫폼 가정이 없도록 유지하면 와이어 포맷을 독립적으로
테스트할 수 있습니다.

### `graftx-transport` (lib)

**게스트 간(guest-to-guest) 채널**을 소유합니다. `Transport` trait —
양방향, framed 바이트 채널 — 을 노출합니다:

```rust
pub trait Transport {
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
    fn recv(&mut self) -> io::Result<Vec<u8>>;
}
```

구체적인 백엔드(**virtio-vsock**, **ivshmem 공유 메모리** — 둘 다 계획됨)는
이 trait를 구현하므로, 클라이언트와 서버는 특정 채널이 아니라 추상화에
대해 작성됩니다. Transport는 **불투명 frame(opaque frame)** 을 옮길 뿐,
프로토콜을 이해하지 않습니다. 백엔드별 unsafe 코드(공유 메모리 매핑, raw
socket handle)는 저장소 관례에 따라 `// SAFETY:` 주석과 함께 전용 모듈에
격리됩니다.

### `graftx-client` (cdylib + rlib)

Linux 쪽의 **API shim**. `cdylib`로 빌드되어 실제 드라이버 라이브러리
(`libvulkan.so`, `libGL.so`, `libEGL.so`, `libcuda.so`, …) 대신 애플리케이션에
로드될 수 있으며, 가로채는 API의 **C 심볼(C symbol)** 을 export합니다. `rlib`
형태는 workspace의 나머지 부분과 테스트가 이를 일반적인 Rust 라이브러리로
링크할 수 있게 합니다.

책임:

- 각 API의 C entry point를 가로챕니다(애플리케이션은 실제 심볼을 봄).
- `graftx-protocol`을 통해 각 호출을 직렬화합니다(`PROTOCOL_VERSION`은 이
  클라이언트가 구사하는 버전이며 handshake에서 서버와 대조됨).
- frame을 `graftx-transport::Transport`를 통해 전달하고, 응답을 기다린 뒤,
  호출자를 위해 반환 값 / 출력 매개변수를 재구성합니다.

모든 심볼 가로채기와 포인터 마샬링은 FFI가 많아 `// SAFETY:` 주석과 함께
전용 `unsafe` 모듈에 한정됩니다. 라이브러리 경로는 `unwrap()` / `expect()`를
피합니다.

### `graftx-server` (bin)

GPU를 소유한 게스트에서 실행되는 Windows 쪽 **재생 엔진(replay engine)**.
다음을 수행합니다:

- 짝지어진(paired) 클라이언트로부터 transport 연결을 수락하고 버전
  handshake를 수행합니다.
- 들어오는 명령 스트림을 `graftx-protocol`을 통해 디코딩합니다.
- 디코딩된 모든 명령을 **검증하고 경계 검사(bounds-check)** 합니다(스트림은
  신뢰할 수 없음 — *보안* 참조).
- 각 호출을 실제 GPU의 **native 벤더 드라이버**에 대해 재생합니다.
- 결과, surface, 상태를 transport를 통해 다시 반환합니다.

이 구성 요소는 공격자가 영향을 줄 수 있는 데이터로 실제 드라이버를 건드리는
부분이므로, 일차적인 sandboxing 및 hardening 대상입니다.

---

## 호출 수명 주기(Call lifecycle)

하나의 전달된 GPU 호출은 여섯 단계를 거칩니다:

1. **Intercept(가로채기)** — 애플리케이션이 API entry point를 호출하고, 실제
   드라이버 대신 로드된 `graftx-client` shim이 인자와 함께 그 호출을
   받습니다. 로컬 전용이거나 손쉽게 캐시 가능한 호출은 왕복(round-trip)
   없이 응답될 수도 있습니다 **(계획)**; 그 외의 모든 것은 전달됩니다.

2. **Encode(인코딩)** — shim은 `graftx-protocol`을 통해 호출을 프로토콜 명령
   frame으로 직렬화합니다: opcode + 마샬링된 인자. 포인터는 인라인
   페이로드(입력 버퍼)나 서버 측 할당 handle(객체, 출력 버퍼) 중 하나로
   변환됩니다.

3. **Transport (send)** — frame은 `Transport::send`에 넘겨져 게스트 간
   채널(virtio-vsock 또는 ivshmem)을 가로질러 Windows 게스트로 전달됩니다.

4. **Decode + validate(디코딩 + 검증)** — 서버는 `Transport::recv`로 frame을
   읽고, `graftx-protocol`을 통해 디코딩한 뒤 이를 **검증**합니다: 알려진
   opcode인지, 길이/handle이 범위 내인지, 버퍼 크기가 타당한지. 유효하지 않은
   frame은 드라이버로 전달되지 않고 거부됩니다(`ProtocolError`).

5. **Replay(재생)** — 서버는 재구성된 인자로 실제 GPU의 해당 native 드라이버
   entry point를 호출하며, 클라이언트 측 handle을 실제 드라이버 객체로
   매핑합니다.

6. **Return(반환)** — 결과, 출력 버퍼, surface가 인코딩되어 transport를 통해
   다시 전송됩니다; 클라이언트는 이를 디코딩하고, 마치 호출이 로컬에서
   실행된 것처럼 애플리케이션에 반환합니다.

```
app → [intercept] → [encode] → [send] ──▶ [recv] → [decode+validate] → [replay] → GPU
                                                                                    │
app ← [decode]   ←  [recv]   ← [send] ◀── [encode results] ◀───────────────────────┘
```

전체를 관통하는 지침이 되는 제약: **결과가 필요한 호출**(동기 조회)은 완전한
왕복 비용이 드는 반면, **fire-and-forget** 호출(대부분의 명령 버퍼 기록)은
파이프라이닝(pipeline)될 수 있습니다. 프로토콜과 shim은 동기적이고 왕복에
묶인 호출을 가능한 한 드물게 유지하도록 설계됩니다(*성능 전략* 참조).

---

## Transport 선택지

`graftx-transport`는 채널을 `Transport` trait 뒤로 추상화하므로, 클라이언트나
서버를 건드리지 않고 배포마다 백엔드를 선택할 수 있습니다. 두 백엔드를
목표로 하며, 둘 다 **계획됨**(trait는 존재하나 구체적 구현은 아직 없음)입니다.

### virtio-vsock

게스트 간의 socket 스타일, 호스트 중재(host-mediated) 채널(`AF_VSOCK`)이며,
context ID + port로 주소가 지정됩니다.

- **장점:** 단순한 스트림/datagram 의미론; 자연스러운 framing과
  backpressure; 간단한 연결 설정과 해제; hypervisor가 이미 제공하는 것 외에
  별도의 커스텀 공유 디바이스가 필요 없음.
- **단점:** 모든 페이로드가 vsock 경로를 통해 **복사**됩니다(게스트 → 호스트
  → 게스트). 이는 GPU 트래픽을 지배하는 대형 버퍼 전송(텍스처, 정점 데이터,
  compute 입력)에 해를 끼칩니다; latency가 공유 매핑보다 높습니다.
- **적합한 용도:** 제어 평면(control-plane) 트래픽, handshake, 작은 명령
  frame, 그리고 초기 구동(bring-up) — 올바른 왕복을 먼저 동작시키기에 가장
  쉬운 백엔드입니다.

### ivshmem 공유 메모리

양쪽 게스트에 매핑되는 공유 메모리 영역(inter-VM shared memory)이며, 신호용
작은 doorbell/ring을 가집니다.

- **장점:** 대량 데이터에 대한 **zero-copy** — 클라이언트가 버퍼를 한 번
  쓰면 서버가 그 자리에서 읽으며 호스트 경유(host bounce)가 없음; 가장 낮은
  latency; GPU 워크로드를 지배하는 대형 surface/buffer 전송에 이상적임.
- **단점:** 더 복잡하고 더 위험함 — 양쪽 끝이 쓰기 가능한 메모리를
  공유하므로 신중한 동기화(ring index, fence)와 검증을 요구함; 더 큰 공격
  표면이자 더 큰 `unsafe` 코드의 원천임.
- **적합한 용도:** 데이터 평면(data plane) — 대량 버퍼, frame, 그리고 vsock
  위에서 정확성이 확립된 뒤의 hot path.

### 트레이드오프 요약

| Aspect            | virtio-vsock              | ivshmem shared memory          |
| ----------------- | ------------------------- | ------------------------------ |
| 데이터 이동        | 복사됨 (guest↔host↔guest) | zero-copy (매핑된 영역)        |
| Latency           | 더 높음                   | 가장 낮음                      |
| 설정 복잡도        | 낮음                      | 더 높음 (ring + doorbell)      |
| 대량 버퍼 비용     | 나쁨                      | 우수함                         |
| 공격 표면          | 더 작음                   | 더 큼 (공유 쓰기 가능 RAM)     |
| 가장 적합한 용도   | 제어 평면 / 초기 구동     | 데이터 평면 / hot path         |

**방향성(계획):** 먼저 virtio-vsock 위에서 정확성을 구동한 뒤 대량 데이터
경로를 ivshmem으로 옮깁니다; 작은 제어 frame은 vsock을 쓰고 큰 버퍼는 공유
메모리를 타는 하이브리드가 정상 상태(steady state)가 될 가능성이 높습니다.

---

## API 커버리지 전략

커버리지가 **최우선 순위**이므로, 전략은 각 API 구현 비용 대비 폭(breadth)을
저울질합니다. 두 가지 구현 방식이 있습니다:

- **Native forward (API별).** API를 가로채 서버에서 *동일한* native API에
  대해 재생합니다(Vulkan → Vulkan, CUDA → CUDA). 가장 높은 충실도와 가장
  낮은 오버헤드를 갖지만, 각 API가 마샬링해야 할 별개의 큰 표면입니다.
- **Funnel-through (변환).** 이미 전달되고 있는 다른 API 위로 한 API를
  변환합니다 — **Zink**(OpenGL → Vulkan)와 **clvk**(OpenCL → Vulkan)가
  취하는 접근입니다. 직렬화된 하나의 백엔드(Vulkan)가 여러 프론트엔드 API를
  운반합니다. 일부 충실도와 오버헤드 비용을 감수하는 대신 API별 마샬링
  작업이 훨씬 적습니다.

GraftX는 **하이브리드 접근**을 취합니다: 가치가 높고 성능이 중요한 API는
native로 전달하고, 성숙한 변환 계층이 존재하는 폭(breadth) API는 이미
전달되고 있는 백엔드로 funnel합니다. **Vulkan이 중추(spine)** 입니다 — 얇고,
명시적이며, 오버헤드가 가장 낮고, 그래픽과 compute 프론트엔드의 자연스러운
funnel 대상입니다.

### 단계적 롤아웃(Tiered rollout)

폭 우선(breadth-first) 목표는 tier로 실현됩니다(README와 로드맵을 반영):

- **Tier 1 — backbone:** **Vulkan**(중추), **OpenGL**, **OpenGL ES**,
  **EGL**, **CUDA**, **OpenCL**. Vulkan이 먼저 native로 전달되고(milestone
  M1); GL/GLES/EGL가 뒤따르며; CUDA와 OpenCL이 cross-vendor compute를
  가져옵니다.
- **Tier 2 — breadth:** **HIP**(AMD/ROCm), **Level Zero**(Intel oneAPI),
  그리고 video codecs — **VA-API**, **VDPAU**, **NVENC/NVDEC**, **Vulkan
  Video**.
- **Tier 3 — reach:** **SYCL**(Level Zero에 올라탐), **OptiX**(CUDA에
  올라탐), **AMF**, **WebGPU/wgpu**, **GLX**. 이들은 이미 커버된 하위 API로의
  funneling에 크게 의존합니다.

한 API가 다른 것에 "올라타는(rides)" 경우(SYCL는 Level Zero 위에, OptiX는
CUDA 위에, 그리고 GL 계열은 Zink 스타일 경로를 통해 Vulkan 위에 **(계획)**),
GraftX는 하위 API가 전달되고 나면 그 커버리지를 대체로 공짜로 얻습니다 —
이것이 바로 Vulkan과 compute 백엔드가 먼저 오는 이유입니다.

---

## 성능 전략

성능은 두 번째 우선 순위입니다. 원격 오버헤드는 두 가지 비용에 지배됩니다:
게스트 경계를 가로지르는 **왕복 latency**와 대형 버퍼의 **데이터 복사**.
전략은 둘 다를 공략합니다. 아래 항목은 모두 **계획됨**입니다.

- **Zero-copy 버퍼.** 대량 데이터(텍스처, vertex/index 버퍼, compute 입력,
  디코딩된 frame)를 ivshmem 공유 영역을 통해 옮겨 vsock 경로로 복사되는
  대신 한 번 쓰고 그 자리에서 읽도록 합니다. GPU 스타일 트래픽에 대한 가장
  큰 단일 지렛대입니다.

- **명령 배칭(Command batching).** 다수의 기록된 호출 — 특히 Vulkan 같은
  명시적 API에서 자연스럽게 지연되는 명령 버퍼 기록 — 을 하나의 transport
  frame으로 합칩니다. frame별 framing과 신호 오버헤드를 많은 호출에 걸쳐
  분산(amortize)합니다.

- **비동기 제출(Asynchronous submission).** fire-and-forget 호출을
  애플리케이션 스레드가 응답을 기다리며 블록되지 않게 전달합니다. 결과를
  *반환하는* 호출(동기 조회, fence, map-readback)만 왕복을 강제합니다; 나머지는
  파이프라이닝되어 클라이언트가 서버보다 앞서 달립니다.

- **왕복 최소화.** 프로토콜과 shim은 hot path에서 동기 조회를 피하도록
  설계됩니다: 객체 handle을 클라이언트 측에서 할당하고 느긋하게(lazily)
  조정(reconcile)하며, API 계약이 허용하는 곳에서 오류/상태 보고를 지연하고,
  불변(immutable) 조회 결과(device 속성, 한계, extension 목록)를 첫 호출
  이후 캐시합니다. 회피한 각 왕복은 임계 경로(critical path)에서 게스트 간
  latency 하나를 제거합니다.

북극성(north star): 정상 상태의 hot path를 **배칭되고, 비동기적이며,
zero-copy** 로 유지하여, 원격 오버헤드가 호출당 latency 세금이 아니라 native
드라이버 시간에 더해지는 작은 상수가 되게 하는 것.

---

## 보안 / 위협 모델

> **아직 hardening되지 않음.** GraftX는 초기 개발 동안 신뢰할 수 없는
> 클라이언트에 **노출되어서는 안 됩니다**. 아래 보호 장치는 의도된 설계이며;
> 일부는 **계획됨**입니다.

핵심 위험: **서버가 신뢰할 수 없는 명령 스트림을 native GPU 드라이버에 대해
재생한다**는 점. Linux 클라이언트 — 따라서 transport로 도착하는 모든 것 —
은 **신뢰할 수 없는 것(untrusted)** 으로 취급됩니다. GPU 드라이버는 크고
복잡하며 권한 있는 C/C++ 코드이고, 공격자가 제어하는 명령과 버퍼를 거기에
먹이는 것은 실제 공격 표면입니다:

- **드라이버 버그** — 잘못 구성된(malformed) 인자가 벤더 드라이버 깊숙한
  곳에서 메모리 안전성 결함을 유발할 수 있습니다.
- **범위 밖(Out-of-bounds) 버퍼** — 스트림 안의 크기, offset, handle은 범위
  내에 있다고 절대 신뢰해서는 안 됩니다.
- **자원 고갈(Resource exhaustion)** — 악의적이거나 결함이 있는 클라이언트가
  GPU 메모리, handle, 또는 서버 CPU/RAM을 고갈시키려 할 수 있습니다.

### 방어

- **디코딩된 모든 명령을 검증하고 경계 검사한다.** 서버는 디코딩된 모든
  입력을 적대적인 것으로 취급합니다: 무엇이든 드라이버에 도달하기 *전에*
  opcode, 길이, offset, handle 참조를 알려진 한계에 대해 검증합니다. 디코딩
  실패는 `ProtocolError`로 표면화되며 해당 명령은 거부되고 결코 전달되지
  않습니다.

- **ivshmem 경로에서는 그 자리(in place)가 아니라 private 복사본을 검증한다.**
  zero-copy ivshmem 영역은 **클라이언트 측에서 쓰기 가능한 채로** 매핑된
  상태로 남아 있으므로, 서버가 그 영역에서 읽는 모든 바이트는 검사 *이후*
  그리고 드라이버 읽기 *이전*(또는 도중)에 클라이언트에 의해 변경될 수
  있습니다. 따라서 그 영역에 대해 데이터를 그 자리에서 검증하는 것은 전형적인
  **이중 페치(double-fetch) / TOCTOU** 위험입니다: 서버가 검증한 값이
  드라이버가 나중에 보는 값과 같다고 보장되지 않습니다. 클라이언트가 쓰기
  가능한 공유 메모리에 대한 in-place 검증은 **안전하지 않으며** 사용해서는 안
  됩니다. 따라서 **권장되는 기본 경로**는 (a)입니다: ivshmem로 도착하는 모든
  명령에 대해 서버는 **반드시** 길이, offset, handle 필드 — 그리고 검증이
  의존하는 모든 보안 관련 버퍼 — 를 **서버 private 메모리**로 복사하고, 그
  private 복사본을 검증한 뒤, 드라이버에는 *오직* 그 private 복사본만
  넘겨야 합니다. 쓰기 회수(b)는 서버가 자신의 뷰를 바꿔서 할 수 있는 일이
  **아닙니다**: ivshmem에서는 각 게스트가 공유 디바이스 BAR를 독립적으로
  매핑하므로, 서버가 *자신의* 매핑을 읽기 전용으로 재매핑해도 클라이언트(별개의
  게스트)가 그 영역에 쓰는 것을 전혀 막지 못합니다. 피어 게스트의 쓰기 접근을
  회수하는 것은 오직 **hypervisor / ivshmem 디바이스 계층에서만** 강제할 수
  있습니다 — 예: QEMU ivshmem 설정이나 BAR의 쓰기 가능성에 대한 호스트 중재
  (host-mediated arbitration)를 통해 — 결코 서버 단독으로는 불가능합니다; 그리고
  설령 가능하더라도 호출당 hot path에서 토글하기에는 비용이 크고 흔히
  실현 불가능합니다. 그러므로 복사 후 검증(a)이 선호됩니다. 이는 대량 데이터
  경로에서 "zero-copy"와 "검증됨(validated)"이 서로 긴장 관계에 있음을
  명시적으로 드러냅니다; 설계는 in-place 데이터를 신뢰하는 것이 **아니라**
  private 복사본을 검증함으로써 그 긴장을 해소합니다. (메커니즘 **계획됨**.)

- **서버를 sandbox한다.** GPU 작업이 허용하는 최소 권한으로 재생 엔진을
  실행하여, 드라이버 침해가 봉쇄되어 Windows 게스트의 나머지나 호스트로
  pivot할 수 없도록 격리합니다. (메커니즘 **계획됨**.)

- **짝지어진 게스트 채널만.** transport를 배포에 해당하는 **특정 짝지어진
  게스트**로만 제한합니다 — virtio-vsock(CID/port 범위 지정)과
  ivshmem(어느 게스트가 영역을 매핑하는지) 모두 임의의 또는 원격 당사자가
  연결할 수 없도록 구성됩니다. 네트워크 모드나 멀티테넌트(multi-tenant)
  모드는 없습니다.

- **버전 handshake.** 클라이언트와 서버는 어떤 GPU 호출이 전달되기 전에
  `PROTOCOL_VERSION`을 협상합니다; 불일치 시 최선의 노력으로 모호하게
  디코딩을 시도하기보다 세션을 중단합니다.

- **unsafe 코드를 봉쇄한다.** 모든 FFI와 raw 메모리 접근(클라이언트 심볼
  가로채기, 서버 드라이버 호출, ivshmem 매핑)은 `// SAFETY:` 주석과 함께
  전용 `unsafe` 모듈에 격리됩니다; 라이브러리 경로는 `unwrap()` / `expect()`를
  피합니다. unsafe 표면을 작고 명시적으로 유지하면 감사(audit) 가능성이
  유지됩니다.

### 신뢰 경계(Trust boundaries)

```
   trusted-by-its-owner            UNTRUSTED stream            replays against
   Linux application      ──────▶  across transport   ──────▶  native drivers
   (its own client shim)           (paired guests)             (server: validate
                                                                + sandbox here)
```

단단한 경계는 **서버의 decode 단계**에 있습니다: 그 이전의 모든 것은 신뢰할
수 없으며, 드라이버가 보는 모든 것은 이미 검증되어 있어야 합니다.

---

## 관례(기여자용)

- Rust edition 2021, `rust-version` 1.75, `rustfmt` + `clippy`를 갖춘 stable
  toolchain. CI gate: `cargo clippy --all-targets -- -D warnings`와
  `cargo fmt --check`.
- 모든 `unsafe` / FFI는 `// SAFETY:` 주석과 함께 전용 모듈에 격리.
- 라이브러리 경로에서 `unwrap()` / `expect()` 금지.
- 클라이언트 shim은 가로채는 API의 C 심볼을 export하는 `cdylib`.
- Conventional Commits (`feat:` / `fix:` / `refactor:` / `docs:` / `chore:` /
  `test:`); 브랜치는 `feature|fix|chore/<name>`; `main`으로 squash-merge.

API 커버리지 표는 [FEATURES.ko.md](FEATURES.ko.md)를, 로드맵은
[design/ROADMAP.md](design/ROADMAP.md)를, 보안 신고는 [SECURITY.ko.md](SECURITY.ko.md)를
참조하세요.
