# kax Rust 재작성 요구사항

2026-11-24 확정. 호환 동작의 기준은 기존 Python·Swift 구현 `e0127d0`과 해당 커밋의 단위 테스트 103개다. 이 문서는 첫 Rust 릴리스의 구현 범위와 인수 기준을 정의한다.

## 실행 환경과 호환성

- macOS에는 Apple Silicon과 Intel을 모두 포함한 Universal 바이너리 `kax`를, Windows에는 x64 바이너리 `kax.exe`를 배포한다. 두 플랫폼 모두 배포된 실행 파일 하나로 CLI와 stdio MCP 서버를 실행할 수 있어야 한다. 사용자에게 Python, Xcode, Rust 도구 체인 설치를 요구하지 않는다.
- macOS 접근성 API 호출과 Windows의 Win32·WinRT OCR 연동까지 Rust로 구현한다. 기존 Swift 브리지는 동작을 대조하는 참조 구현으로 사용한다.
- 기존 CLI의 명령, 인자, JSON 출력, 종료 코드를 유지한다. MCP 도구 13개의 이름, 입력·출력 스키마, 오류 의미와 각 OS에서 지원하는 동작을 보존한다.
- Python 공개 API `kakaotalk_automation.NativeAX`와 스킬의 Python 진입점 `scripts/kax.py`는 지원을 종료한다. 저장소의 카카오톡 스킬과 사용 예시는 설치된 `kax`를 직접 호출하도록 전환한다.

## 전송 저널의 단방향 이전

구버전 CLI와 MCP 프로세스를 종료한 뒤 기존 전송 기록을 Rust판으로 한 번 이전한다. Rust판은 이전된 `operation_id`를 알아보고 기존 요청에 대한 결과 조회와 중복 전송 방지를 이어간다. Rust판에서 생성한 기록을 구버전으로 다시 가져가는 기능은 지원 범위에서 제외한다.

예를 들어 Python판이 전송 의도를 기록한 뒤 결과를 확정하기 전에 종료된 경우, Rust판에서 같은 대상·본문·`operation_id`로 호출하면 결과를 확인하고 전송 동작을 다시 수행하지 않는다. 같은 ID를 다른 대상이나 본문으로 재사용하면 기존과 같은 오류를 반환한다.

## 검증과 첫 릴리스 통과 기준

| 단계 | 통과 기준 |
|---|---|
| 실기기 사용 전 CI | 기존 103개 테스트가 검증하던 동작을 Rust 테스트에 대응시킨다. 기존의 모의 Win32·클립보드와 합성 접근성 트리 방식으로 양쪽 OS에서 해당 플랫폼의 동작을 검증한다. 저널 이전과 미확정 요청의 중복 전송 방지도 검증한다. 별도의 카카오톡 GUI 목 앱은 만들지 않는다. |
| 배포 바이너리 확인 | macOS 바이너리에 Apple Silicon과 Intel 코드가 모두 포함됐음을 CI에서 확인한다. Windows 배포 대상은 x64다. |
| 실기기 스모크 | 로컬 Mac과 Windows PC(`win-pc`)에서 각각 한 차례 배포용 바이너리로 CLI와 MCP를 실행한다. 각 OS에서 채팅방 찾기, 메시지 읽기, 검색, 이미지 첨부 읽기를 확인한다. |
| 실제 전송과 재호출 | 각 기기에서 식별 가능한 테스트 메시지를 ‘나와의 채팅’에만 보낸다. 같은 `operation_id`로 재호출한 뒤 해당 메시지가 대화창에 정확히 한 건 남아야 한다. |

실기기 결과에는 실제로 검증한 기종과 OS 버전을 기록한다. 첫 Rust판은 양쪽 스모크를 모두 통과한 뒤 `main`에 병합한다.

## 설치와 자동 배포

- **GitHub Releases**에서 macOS Universal 및 Windows x64 바이너리를 직접 내려받을 수 있게 한다.
- **설치 스크립트**는 macOS용 `install.sh`와 Windows용 `install.ps1`을 제공한다. 각 스크립트는 다운로드한 배포 파일의 체크섬을 검증한 뒤 설치한다.
- **crates.io**에도 배포해 Rust 도구 체인이 있는 사용자가 `cargo install`로 설치할 수 있게 한다.
- **GitHub Actions**는 배포 대상 코드 변경이 `main`에 반영되면 필요에 따라 패치 버전을 올리고 GitHub Releases와 crates.io에 자동 배포한다.

## 저장소 내 참조

- [기존 CLI·MCP 사용법](../README.md)
- [Windows 지원 동작과 플랫폼 제약](windows-support.md)
- [기존 단위 테스트](../tests/)
- [카카오톡 에이전트 스킬](../skills/kakaotalk-ax/SKILL.md)
