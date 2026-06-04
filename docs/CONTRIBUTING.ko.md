# GraftX에 기여하기

[English](../CONTRIBUTING.md) | **한국어**

GraftX에 기여하는 데 관심을 가져 주셔서 감사합니다!

GraftX는 GPU API 리모팅 계층입니다. Linux 게스트의 GPU API 호출(OpenGL,
OpenGL ES, EGL, Vulkan, CUDA, OpenCL, ROCm/HIP, Level Zero, 비디오 코덱 등)을
클라이언트 shim이 가로채어 직렬화하고, 빠른 게스트 간 transport(virtio-vsock /
ivshmem)를 통해 전송하면, PCI passthrough로 물리 GPU를 소유한 Windows 게스트의
서버가 이를 재생(replay)합니다. 결과와 surface는 다시 Linux 측으로 반환됩니다.

이 프로젝트는 **초기 개발 단계**(버전 `0.0.0`)에 있으며, 아직 안정적인 것은
없습니다. 프로젝트의 우선순위는 다음 순서대로입니다.

1. API 커버리지의 폭
2. 성능(리모팅 오버헤드 최소화)
3. 안정성
4. 안전성

변경을 제안할 때 이 우선순위를 염두에 두시기 바랍니다.

## 사전 준비물

- [rustup](https://rustup.rs/)을 통해 설치한 **Rust (stable)**. 이 프로젝트는
  edition 2021을 대상으로 하며 최소 `rust-version`은 `1.75`입니다.
- `rustfmt` 및 `clippy` 컴포넌트:

  ```sh
  rustup component add rustfmt clippy
  ```

### 워크스페이스 구조

GraftX는 네 개의 crate로 구성된 Cargo workspace입니다.

| Crate              | 종류            | 책임                                                                      |
| ------------------ | --------------- | ------------------------------------------------------------------------- |
| `graftx-protocol`  | lib             | 와이어 포맷, 명령 encode/decode, `PROTOCOL_VERSION`, `ProtocolError`.     |
| `graftx-transport` | lib             | vsock / ivshmem 위에서 동작하는 `Transport` trait (send/recv).            |
| `graftx-client`    | cdylib + rlib   | Linux API shim (`libvulkan.so`, `libGL.so`, `libcuda.so`, ...).           |
| `graftx-server`    | bin             | Windows 측에서 네이티브 GPU 드라이버에 대한 replay.                       |

## 빌드 및 테스트

모든 명령은 워크스페이스 루트에서 실행합니다.

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

pull request를 열기 전에 브랜치가 깨끗하게 빌드되고 위 네 가지를 모두
통과해야 합니다.

## 코드 스타일

- **포매팅**: rustfmt 기본값. 커밋하기 전에 `cargo fmt`를 실행하세요. CI는
  `cargo fmt --check`를 강제합니다.
- **린팅**: clippy는 경고를 거부한 상태
  (`cargo clippy --all-targets -- -D warnings`)에서 통과해야 합니다.
  `#[allow(...)]`에 대한 명확하고 문서화된 이유가 없는 한, 린트를 억제하기보다
  수정하세요.
- **Unsafe / FFI 격리**: 모든 `unsafe` 및 FFI 코드는 전용 모듈에 둡니다. 모든
  `unsafe` 블록에는 불변식이 왜 성립하는지 설명하는 `// SAFETY:` 주석을 달아야
  합니다. unsafe 노출 영역은 최소화하고 한정되도록 유지하세요.
- **라이브러리 경로에서 `unwrap()` / `expect()` 금지**: 라이브러리 코드
  (`graftx-protocol`, `graftx-transport`, 그리고 `graftx-client` /
  `graftx-server`의 라이브러리/런타임 경로)는 panic 대신 `Result`로 오류를
  전파해야 합니다. `unwrap` / `expect`는 테스트, 예제, 일회성 도구에서만
  허용됩니다.
- **클라이언트 shim**은 가로채는 API의 C 심볼을 export하는 `cdylib` crate
  입니다. 상위(upstream) C ABI와 정확히 일치시키세요.

### 보안에 민감한 코드

서버는 네이티브 GPU 드라이버에 대해 **신뢰할 수 없는** 명령 스트림을 재생하며,
이는 실질적인 공격 표면(드라이버 버그, 범위를 벗어난 버퍼, 자원 고갈)입니다.
클라이언트를 신뢰할 수 없는 것으로 취급하세요. 명령을 검증하고 경계 검사를 하며,
와이어 데이터가 올바른 형식이라고 절대 가정하지 마세요. 프로토콜 디코딩,
transport, replay 코드를 건드리기 전에 전체 위협 모델은
[`SECURITY.ko.md`](SECURITY.ko.md)를 참고하세요.

## 커밋 규약

- **Conventional Commits**: 각 커밋 제목 앞에 `feat:`, `fix:`, `refactor:`,
  `docs:`, `chore:`, `test:` 중 하나를 붙이세요.

  ```text
  feat(protocol): add vkCreateBuffer command encoding
  fix(transport): handle short reads on vsock recv
  ```

- **브랜치 명명**: `feature/<name>`, `fix/<name>`, 또는 `chore/<name>`을
  사용하세요.
- **머지**: pull request는 `main`으로 **squash-merge**됩니다. PR 제목은 squash
  커밋 제목이 되므로 Conventional Commit 형식을 유지하세요.

### AI 출처 표기 금지 정책

이는 **엄격한 저장소 정책**입니다. 커밋과 pull request에는 어떠한 AI 또는
어시스턴트 출처 표기도 포함되어서는 **안 됩니다**. 구체적으로 다음을 포함하지
**마세요**.

- `Co-Authored-By: Claude` (또는 모든 AI/어시스턴트 공동 작성자 트레일러)
- `Generated with [Claude Code]` (또는 모든 "made with AI" 푸터 / 링크)
- 모든 로봇 이모지 또는 유사한 "made with AI" 표식

이는 커밋 메시지, PR 제목, PR 설명, 이슈 제목 및 본문, 코드 주석, GitHub 댓글,
CHANGELOG 항목, 릴리스 노트에 적용됩니다. 기여자는 자신의 기여에 대한 유일한
인간 작성자입니다. AI 출처 표기가 포함된 PR은 머지 전에 제거를 요청받게 됩니다.

## Pull request 절차

1. 위의 브랜치 명명 규칙을 사용하여 저장소를 fork하세요(쓰기 권한이 있으면
   브랜치를 생성하세요).
2. 변경 사항을 집중적이고 논리적으로 범위가 한정된 커밋으로 만드세요.
3. 전체 로컬 점검 스위트(build, test, clippy, fmt)를 실행하여 통과시키세요.
4. Conventional Commit 형식의 제목과 무엇을 왜 변경했는지에 대한 명확한 설명을
   담아 `main`을 대상으로 PR을 여세요.
5. 리뷰 피드백을 반영하세요. PR은 승인되면 squash-merge됩니다.

### PR 체크리스트

리뷰를 요청하기 전에 다음을 확인하세요.

- [ ] `cargo build --workspace`가 성공한다.
- [ ] `cargo test --workspace`가 통과한다.
- [ ] `cargo clippy --all-targets -- -D warnings`가 깨끗하다.
- [ ] `cargo fmt --check`가 diff를 보고하지 않는다.
- [ ] 라이브러리 경로에 `unwrap()` / `expect()`를 추가하지 않았다.
- [ ] 모든 새로운 `unsafe` / FFI가 격리되어 있고 `// SAFETY:` 주석을 담고 있다.
- [ ] 보안에 민감한 변경은 신뢰할 수 없는 명령 스트림을 고려한다
      ([`SECURITY.ko.md`](SECURITY.ko.md) 참고).
- [ ] PR과 그 커밋에 **AI 출처 표기가 없다**(위 정책 참고).
- [ ] 커밋 제목과 PR 제목이 Conventional Commits를 따른다.

## 행동 강령

이 프로젝트에 참여함으로써 귀하는
[`CODE_OF_CONDUCT.ko.md`](CODE_OF_CONDUCT.ko.md)를 준수하는 데 동의하는 것입니다.

## 보안

보안 취약점에 대해서는 공개 이슈를 제출하지 마세요.
[`SECURITY.ko.md`](SECURITY.ko.md)의 보고 절차를 따르세요. GraftX는 **아직 강화되지
않았으며**, 초기 개발 중에는 신뢰할 수 없는 클라이언트에 노출되어서는 안 됩니다.
