# kakaotalk-ax

macOS와 Windows의 공식 카카오톡 앱에서 채팅방 탐색, 본문·사진 읽기, 검색, 메시지 전송을 제공하는 단일 네이티브 바이너리(`kax`) CLI·MCP 서버입니다.
`kax` 실행 파일 하나로 macOS에서는 Accessibility(AX)·ScreenCaptureKit·Vision OCR을, Windows에서는 Win32 API·WinRT OCR을 직접 호출합니다. Python, Xcode, Rust 도구 체인 설치가 필요하지 않습니다.
카카오톡 DB·프로세스 메모리·네트워크 데이터에는 접근하지 않습니다.

## 설치

### macOS (Apple Silicon + Intel Universal)

macOS 설정에서 실행하는 터미널 또는 에이전트에 **손쉬운 사용(Accessibility)** 권한을 부여해야 합니다. 사진 읽기(`read_attachment`)는 macOS 14.2 이상과 **화면 기록** 권한이 필요합니다.

```bash
curl -fsSL https://raw.githubusercontent.com/hyunseob/kakaotalk-ax/main/install.sh | bash
```

설치가 끝나면 새 터미널 창을 열고 실행합니다.

```bash
kax status
```

`install.sh`는 GitHub Releases에서 macOS Universal 바이너리와 `checksums.txt`를 내려받아 SHA-256 체크섬을 검증한 뒤 `~/.local/bin/kax`에 설치합니다. `~/.local/bin`이 `PATH`에 없으면 로그인 셸의 시작 파일(zsh는 `~/.zshrc`, bash는 `~/.bash_profile`)에 `export PATH="$HOME/.local/bin:$PATH"` 한 줄을 자동으로 추가하므로, 이후 새 터미널에서는 바로 `kax`를 사용할 수 있습니다.

설치 스크립트는 하위 셸에서 실행되므로 설치를 실행한 현재 터미널의 `PATH`는 바꾸지 못합니다. 새 터미널을 열지 않고 현재 창에서 바로 사용하려면 다음을 실행합니다.

```bash
export PATH="$HOME/.local/bin:$PATH"
```

셸 시작 파일을 직접 관리하려면 `KAX_NO_MODIFY_PATH=1`을 지정해 자동 추가를 끕니다. 이때 스크립트는 파일을 수정하지 않고 추가할 줄만 출력합니다.

```bash
curl -fsSL https://raw.githubusercontent.com/hyunseob/kakaotalk-ax/main/install.sh | KAX_NO_MODIFY_PATH=1 bash
```

### Windows (x64)

Windows 10 또는 11, 같은 데스크톱 세션에서 실행 중인 공식 카카오톡 PC가 필요합니다.
방 목록과 사진 OCR에는 Windows 설정에서 **한국어 언어팩의 광학 문자 인식**을 설치합니다.

```powershell
irm https://raw.githubusercontent.com/hyunseob/kakaotalk-ax/main/install.ps1 | iex
kax build
kax status
```

`install.ps1`은 GitHub Releases에서 Windows x64 바이너리와 `checksums.txt`를 내려받아 SHA-256 체크섬을 검증한 뒤 `%LOCALAPPDATA%\kakaotalk-ax\bin\kax.exe`에 설치합니다.

### Cargo로 설치

Rust 도구 체인이 있으면 macOS와 Windows에서 저장소 소스로 설치할 수 있습니다.

```bash
cargo install --locked --git https://github.com/HyunSeob/kakaotalk-ax.git
```

crates.io에 원하는 버전이 게시된 뒤에는 `cargo install --locked kakaotalk-ax`도 사용할 수 있습니다.

### 구버전(Python) 전송 저널 이전

구버전 CLI와 MCP 프로세스를 종료한 뒤 Rust판에서 처음 `send_message`나 `send_result`를 호출하면 기존 전송 기록(`~/.config/kakaotalk-reader/ax-send-operations` 또는 `%LOCALAPPDATA%\kakaotalk-ax\ax-send-operations`)이 자동으로 한 번 이전됩니다. 수동으로 먼저 이전하려면 다음 명령을 실행합니다.

```bash
kax migrate-journal
```

이전된 `operation_id`는 기존 요청의 결과 조회(`send_result`)와 중복 전송 방지를 그대로 이어갑니다. 이전 결과가 `partial`이면 해당 ID의 전송과 결과 조회가 차단됩니다. `sourceDir`의 손상된 원본 기록을 백업에서 복구한 뒤 `kax migrate-journal`을 다시 실행하세요.

## CLI

```bash
kax status
kax ax-windows
kax ax-read --window-title '정확한 방 이름' --limit 50
kax ax-call read_many --args-file /absolute/path/arguments.json
kax ax-call send_message --args-file /absolute/path/send.json
kax ax-call send_result --args-file /absolute/path/result.json
```

`ax-call`은 UTF-8 JSON 객체 파일로 인수를 받습니다. 사용자 텍스트는 셸 인자 대신 JSON 파일로 전달하세요.
자세한 메서드·도구 대응과 인수는 [AX API](skills/kakaotalk-ax/references/ax-api.md)를 참고하세요.

## MCP 서버

단일 실행 파일 `kax mcp`로 stdio MCP 서버를 실행합니다. GUI 클라이언트에서 `kax`를 찾지 못하면 아래 `command`에 실행 파일의 절대 경로를 넣으세요. 설치 경로는 macOS에서 `command -v kax`, Windows에서 `(Get-Command kax).Source`로 확인합니다.

```json
{
  "mcpServers": {
    "kakaotalk-ax": {
      "command": "kax",
      "args": ["mcp"]
    }
  }
}
```

| 기능 | 도구 |
|---|---|
| 열린 창 확인 | `kakaotalk_ax_windows` |
| 보이는 채팅방 목록 | `kakaotalk_ax_rooms` |
| 앱의 방 검색 | `kakaotalk_ax_search_rooms` |
| 방 열기 / 창 닫기 | `kakaotalk_ax_open` / `kakaotalk_ax_close` |
| 이미지 첨부 읽기 | `kakaotalk_ax_read_attachment` |
| 한 방 / 여러 방 읽기 | `kakaotalk_ax_read` / `kakaotalk_ax_read_many` |
| 이전 대화 요청 | `kakaotalk_ax_load_more` |
| 본문 검색 / 검색 닫기 | `kakaotalk_ax_search` / `kakaotalk_ax_close_search` |
| 전송 / 결과 확인 | `kakaotalk_ax_send` / `kakaotalk_ax_send_result` |

총 13개 AX 도구를 제공합니다. 저장소의 [카카오톡 에이전트 스킬](skills/kakaotalk-ax/SKILL.md)은 설치된 `kax` CLI와 MCP를 직접 사용합니다.

## 메시지 전송과 안전장치

- 명시적으로 요청받은 방과 본문에만 전송하세요. 요청별 `operation_id`(1~128자 영문·숫자·`_.:-`)를 한 번 정하고 보관합니다.
- Windows에서 전송할 때는 이미 열린 창의 `GetWindowTextW` 원문 제목을 사용합니다. macOS는 전송 전에 방을 열 수 있습니다.
- 본인 채팅은 `open_room(window_title, require_self=true)`로 열고, 전송에도 `require_self=true`를 지정합니다.
- 동명 방·기존 초안·확인할 수 없는 대상은 중단합니다. Windows는 로그인·잠금·모달 상태와 사용자 실시간 입력(`user_input_active`)도 확인합니다.
- 전송 요청은 본문 해시와 함께 본인 전용 파일(`0600`)에 먼저 기록합니다. 같은 ID는 다시 보내지 않으며, 다른 방이나 본문에 재사용하면 거부합니다.
- `observed_in_ax`는 새 본문과 빈 입력창을 관찰했다는 뜻이며 작성자·서버 ACK·읽음 확인이 아닙니다. `outcome_unknown`이나 타임아웃에서는 새 ID로 재발송하지 말고 원래 ID로 `send_result`만 조회하세요.
- 파일 전송·메시지 수정·삭제·채팅방 나가기는 제공하지 않습니다.

## 읽기 범위와 제한

- macOS 방 목록의 이름에는 `오픈채팅, `·`팀채팅, ` 같은 방 종류가 붙어 있지만 창 제목에는 없습니다. `rooms`·`search_rooms` 항목은 원래 `name`과 함께 접두어를 뗀 `windowTitle`, 방 종류 `roomType`(`open_chat`·`team_chat`·`null`)을 돌려줍니다. `open_room`은 두 형태를 모두 받고 접두어를 뗀 이름이 정확히 같은지로 비교하며, 떼고 나서 같은 이름이 둘 이상이면 `target_room_name_ambiguous`로 중단합니다. 요청에 방 종류를 적으면(`"팀채팅, A"`) 목록 항목의 종류가 같을 때만 열고, 그런 방이 없으면 `room_type_mismatch`입니다. 창 제목에는 종류가 없으므로 다른 종류의 같은 이름 방이 목록에 있고 그 이름의 창이 이미 열려 있으면 `target_room_window_ambiguous`로 막습니다. `read_many`·`search_messages`는 `open_room`이 확인한 실제 창 제목으로 읽고 닫으며, 결과의 `windowTitle`은 실제 제목, `requestedTitle`은 요청한 이름입니다.
- `rooms`는 현재 보이는 행을 읽습니다. Windows 목록의 방 이름·미리보기는 `source=windows_ocr`, `textExact=false`이며 안 읽음 표시는 빨간 배지 픽셀을 판독합니다.
- `read_messages`는 열린 창의 macOS AX 노출 행 또는 Windows 클립보드 원문에서 최대 1000행을 수집합니다. 한 페이지에 반환하는 행 수(`limit`)는 1~500입니다.
- `hasMore`는 수집한 스냅샷 안의 다음 페이지입니다. `nextOffset`과 `snapshotId`를 다음 호출에 전달하고, `snapshot_changed`면 첫 페이지부터 다시 읽으세요.
- `read_attachment`는 표시된 사진 영역을 PNG로 반환합니다. 기본값은 `ocr=false`이며 필요할 때만 `ocr=true`를 지정합니다.
- `load_more`와 `search_messages`는 제한된 범위의 UI 탐색이며 전체 과거 기록의 완전성을 보장하지 않습니다(`historyComplete=false`, `endOfHistory=null`).

## 개발 및 검증

```bash
cargo test
```

단위 테스트에는 로그인된 앱이 필요하지 않습니다. 모의 Win32·클립보드와 합성 접근성 트리, 저널 이전 테스트로 양쪽 OS의 동작을 검증합니다.

## 라이선스

[MIT](LICENSE). 카카오톡 이용약관을 지키는지, 다른 사람의 대화를 어떻게 다루는지는 쓰는 사람의 책임입니다.
