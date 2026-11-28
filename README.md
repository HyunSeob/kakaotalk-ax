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
| 채팅방 폴더 목록 | `kakaotalk_ax_folders` |
| 앱의 방 검색 | `kakaotalk_ax_search_rooms` |
| 방 열기 / 창 닫기 | `kakaotalk_ax_open` / `kakaotalk_ax_close` |
| 단체방 대화상대 목록 | `kakaotalk_ax_members` |
| 이미지 첨부 읽기 | `kakaotalk_ax_read_attachment` |
| 한 방 / 여러 방 읽기 | `kakaotalk_ax_read` / `kakaotalk_ax_read_many` |
| 이전 대화 요청 | `kakaotalk_ax_load_more` |
| 본문 검색 / 검색 닫기 | `kakaotalk_ax_search` / `kakaotalk_ax_close_search` |
| 전송 / 결과 확인 | `kakaotalk_ax_send` / `kakaotalk_ax_send_result` |

총 15개 AX 도구를 제공합니다. 저장소의 [카카오톡 에이전트 스킬](skills/kakaotalk-ax/SKILL.md)은 설치된 `kax` CLI와 MCP를 직접 사용합니다.

## 메시지 전송과 안전장치

- 명시적으로 요청받은 방과 본문에만 전송하세요. 요청별 `operation_id`(1~128자 영문·숫자·`_.:-`)를 한 번 정하고 보관합니다.
- Windows에서 전송할 때는 이미 열린 창의 `GetWindowTextW` 원문 제목을 사용합니다. macOS는 전송 전에 방을 열 수 있습니다.
- 본인 채팅은 `open_room(window_title, require_self=true)`로 열고, 전송에도 `require_self=true`를 지정합니다.
- 동명 방·기존 초안·확인할 수 없는 대상은 중단합니다. Windows는 로그인·잠금·모달 상태와 사용자 실시간 입력(`user_input_active`)도 확인합니다.
- 전송 요청은 본문 해시와 함께 본인 전용 파일(`0600`)에 먼저 기록합니다. 같은 ID는 다시 보내지 않으며, 다른 방이나 본문에 재사용하면 거부합니다.
- `observed_in_ax`는 새 본문과 빈 입력창을 관찰했다는 뜻이며 작성자·서버 ACK·읽음 확인이 아닙니다. 이모지 하나만 보낸 경우는 큰 이미지 행으로 관찰하며 `reason`이 `new_emoji_image_row_and_empty_composer`입니다. `outcome_unknown`이나 타임아웃에서는 새 ID로 재발송하지 말고 원래 ID로 `send_result`만 조회하세요.
- 파일 전송·메시지 수정·삭제·채팅방 나가기는 제공하지 않습니다.

## 읽기 범위와 제한

- macOS 방 목록의 이름에는 `오픈채팅, `·`팀채팅, `·`나와의 채팅, ` 같은 방 종류가 붙어 있지만 창 제목에는 없습니다. `rooms`·`search_rooms` 항목은 원래 `name`과 함께 접두어를 뗀 `windowTitle`, 방 종류 `roomType`(`open_chat`·`team_chat`·`self_chat`·`null`)을 돌려줍니다. 나와의 채팅 행에는 배지가 없어서, `나와의 채팅, ` 접두어가 있고 인원 수가 없는 행을 `isSelf: true`로 봅니다(같은 이름으로 지은 단체방은 인원 수가 있어 제외됩니다). 이 `isSelf`는 목록 표시만 보고 정한 참고값입니다. 1:1방이나 친구 이름을 `나와의 채팅, …`으로 바꾸면 잘못 표시될 수 있지만, `require_self=true`는 연 창의 제목까지 확인하므로 그런 방을 열지 않습니다. 내 이름과 같은 이름의 친구가 있으면 두 창의 제목이 같아서, 이름만으로 열면 `target_room_name_ambiguous`로 막힙니다. `open_room`은 두 형태를 모두 받고 접두어를 뗀 이름이 정확히 같은지로 비교하며, 떼고 나서 같은 이름이 둘 이상이면 `target_room_name_ambiguous`로 중단합니다. 요청에 방 종류를 적으면(`"팀채팅, A"`) 목록 항목의 종류가 같을 때만 열고, 그런 방이 없으면 `room_type_mismatch`입니다. 창 제목에는 종류가 없으므로 다른 종류의 같은 이름 방이 목록에 있고 그 이름의 창이 이미 열려 있으면 `target_room_window_ambiguous`로 막습니다. `read_many`·`search_messages`는 `open_room`이 확인한 실제 창 제목으로 읽고 닫으며, 결과의 `windowTitle`은 실제 제목, `requestedTitle`은 요청한 이름입니다.
- 알림톡·비즈니스 채널처럼 입력창이 없는 단방향 채널 창(macOS)도 대화 표가 하나면 `windows`에서 `readable=true`, `hasComposer=false`이고 `open`·`read`·`close`를 쓸 수 있습니다. 초안이 있을 수 없으므로 `close`는 바로 닫습니다(`hadComposer=false`). 단, 입력창이 없는 창은 대화 표가 정확히 하나인 경우만 닫고, 이름표를 모르는 입력칸에 글이 있으면 `existing_draft_preserved`로 막습니다. `open`은 입력창 없는 모양이 0.5초 동안 그대로일 때만 단방향 창으로 보고 `hasComposer=false`를 붙입니다(창을 그리는 중인 일반 채팅방을 잘못 받아들이지 않도록). `windows` 항목에는 `composerCount`도 있습니다. 대화 표도 없으면 `channel_window_unreadable`, 전송은 `chat_composer_unavailable`입니다. `open`이 창을 열었지만 쓸 수 없는 창이면 결과에 `windowOpened=true`가 붙고, `read_many`는 그 창을 `close_after`와 관계없이 다시 닫습니다.
- `members`는 열린 단체방(일반·팀·오픈채팅) 창의 `대화상대 N명` 버튼으로 목록을 열어 대화상대를 읽고 다시 닫습니다(macOS). 항목은 `name`, `rowIndex`, `isSelf`, `role`(`owner`=방장·팀장, `staff`=부방장, `null`), `badges`이고, `memberCount`(버튼의 인원 수)와 `complete`로 전부 읽었는지 확인합니다. 읽지 못한 행은 `skippedRows`로 세고, 한 행이 읽기 한도(`KAX_READ_TIMEOUT_SECS`)를 넘기면 그때까지 읽은 대화상대와 함께 `stopReason=member_row_stalled`로 끝납니다. 목록이 열려 있는 동안에도 같은 방의 읽기·검색은 목록 팝오버를 대화 표로 착각하지 않습니다. 화면 밖 행도 한 명에 약 15ms라 스크롤하지 않습니다. 1:1방·나와의 채팅은 `member_list_unavailable`, Windows는 `member_list_unsupported`입니다.
- 채팅방 폴더(macOS): `folders`는 폴더 이름·폴더별 안 읽은 메시지 수·선택된 폴더를 돌려주고, `rooms(folder="안읽음")`은 그 폴더로 바꿔 읽은 뒤 원래 폴더로 되돌립니다(`folderChanged`, `folderRestored`). 폴더 전환은 `AXSelectedChildren`으로 하며 마우스를 쓰지 않고, 방 목록이 실제로 바뀐 것을 확인한 뒤에 읽습니다(확인되지 않으면 `room_folder_reload_not_observed`). 폴더를 바꾼 뒤에는 읽기가 실패해도 원래 폴더로 되돌리고(최대 3초, 호출 한도를 조금 넘을 수 있음) 그 결과를 `folderRestored`로 알립니다. 원래 폴더의 스크롤 위치도 되돌립니다. 현재 폴더를 알 수 없으면 아무것도 바꾸지 않고 `room_folder_selection_unknown`입니다. 폴더 탭의 수를 못 읽으면 `unread`는 `null`이고, 방 항목의 `unread`·`muted`·`pinned`도 행 표시를 해석하지 못하면 `null`입니다. 방 검색어가 들어가 있으면 `room_search_active`, 없는 폴더는 `folder_not_found`, Windows는 `folder_unsupported`입니다. `rooms` 항목에는 행에 표시된 `unread`(안 읽은 수), `memberCount`, `lastMessageTime`, `muted`(알림 끄기), `pinned`(핀)도 있습니다. 안 읽은 방은 `rooms(folder="안읽음")`으로 고른 뒤 `read_many`로 읽으면 됩니다(방을 열면 읽음 처리됩니다).
- 메인 창 채팅방 검색 바(macOS): `rooms`·`search_rooms` 결과의 `searchOpen`이 검색 바가 열려 있는지 알려 줍니다. `search_rooms(query="")`는 검색어를 지우고 검색 바도 닫습니다(헤더의 검색 토글 버튼, 이미 닫혀 있으면 그대로). `open_room`(그리고 이를 쓰는 `read_many`·`search_messages`)이 방 검색을 쓴 뒤에는 원래 상태로 되돌립니다. 검색 바가 닫혀 있었으면 닫고, 열려 있었으면 원래 검색어로 열어 둡니다(`roomSearchWasOpen`, `roomSearchRestored`는 검색어와 열림 상태가 모두 같아야 `true`). 방 검색 결과가 화면에 보이는 행보다 많으면(예: 이름을 짓지 않은 단체방 `A, B`는 짧은 멤버 이름 때문에 다른 방도 많이 걸립니다) 결과 목록을 끝까지 스크롤해 읽고, 이름이 정확히 같은 방이 하나일 때만 그 행을 화면에 보이게 해서 엽니다. 같은 이름이 둘 이상이면 `target_room_name_ambiguous`, 결과를 끝까지 읽지 못했으면 `room_search_results_incomplete`로 멈추고, 옮긴 스크롤은 되돌립니다(`roomSearchScroll`에 읽은 결과 수와 연 행 번호가 나옵니다).
- `rooms`는 현재 보이는 행을 읽습니다. macOS에서 `scroll=true`를 주면 채팅 목록을 맨 위부터 한 페이지씩 스크롤하며 최대 `limit`개(기본 300, 최대 1000)를 모으고, 끝나면 원래 스크롤 위치로 되돌립니다(그동안 목록이 움직입니다). 결과의 `stopReason`(`complete`·`limit`·`end_of_list`·`no_new_rows`·`user_scrolled`·`tool_call_time_limit`), `historyComplete`, `listChanged`(도중에 새 메시지로 목록이 바뀜), `scrollRestored`로 끝까지 읽었는지 판단합니다. 화면 밖 행을 스크롤 없이 읽으면 행마다 AX 요청이 수 ms씩 걸려 훨씬 느립니다(방 695개 기준 이름만 20초, 스크롤은 약 6초). Windows는 `scroll`을 지원하지 않습니다(`room_list_scroll_unsupported`). Windows 목록의 방 이름·미리보기는 `source=windows_ocr`, `textExact=false`이며 안 읽음 표시는 빨간 배지 픽셀을 판독합니다.
- `read_messages`는 열린 창의 macOS AX 노출 행 또는 Windows 클립보드 원문에서 최대 1000행을 수집합니다. 한 페이지에 반환하는 행 수(`limit`)는 1~500입니다.
- `hasMore`는 수집한 스냅샷 안의 다음 페이지입니다. `nextOffset`과 `snapshotId`를 다음 호출에 전달하고, `snapshot_changed`면 첫 페이지부터 다시 읽으세요.
- `read_attachment`는 표시된 사진 영역을 PNG로 반환합니다. 기본값은 `ocr=false`이며 필요할 때만 `ocr=true`를 지정합니다.
- macOS `read` 항목에는 메시지 정보가 붙습니다: 보이는 이름 `sender`, 내 메시지 `isSelf`, 이어받은 보낸 사람 `groupSender`와 근거 `senderSource`, 묶음 시각 `groupTimeText`, 날짜 `date`·`dateText`, `unreadCount`, `edited`, `truncated`(500자에서 잘림), 파일(`kind: file`, `file.name`·`size`·`expiresAt`), 시스템 알림(`kind: system`, `systemType`·`target`), 날짜 구분선(`kind: date_separator`). `textMessageCount` 등 개수는 이전 버전과 같은 분류로 셉니다. 내 메시지는 이름 없이 묶음을 시작하는 행만 말풍선 위치로 확인하므로 읽기 시간이 조금 늘어납니다(측정 기준 50~60행에 0.1~0.4초).
- `load_more`는 기본으로 요약만 돌려줍니다(늘어난 행 수, 단계별 행 수, `oldestDate`·`newestDate`, 새 `snapshotId`, `rowsTruncated`·`truncatedBefore`·`warning`). 본문은 그 `snapshotId`로 `read(offset, limit)`를 나눠 읽고, 예전처럼 본문까지 받으려면 `include_items=true`를 줍니다. `read`도 1000행 제한으로 오래된 행이 빠지면 `coverage.truncatedBefore`(이 날짜보다 앞선 행이 빠졌고, 이 날짜 구분선 위의 날짜 없는 행은 일부만 있을 수 있음)와 맨 위 `warning`을 줍니다.
- 메인 창이나 채팅 창을 잠깐 찾지 못하는 일시적 실패(`main_window_not_unique`, 읽기 중 `target_room_window_not_open`)는 UI를 건드리기 전이면 0.3초 뒤 한 번 더 시도합니다(`retriedAfter`). 전송(`send`·`send_result`)은 방 열기와 확인 읽기까지 다시 시도하지 않습니다. `accessibility_permission_required`에는 켜야 할 곳을 알려 주는 `hint`와 실제 실행 파일 `executablePath`가 붙습니다.
- `load_more`와 `search_messages`는 제한된 범위의 UI 탐색이며 전체 과거 기록의 완전성을 보장하지 않습니다(`historyComplete=false`, `endOfHistory=null`).
- macOS AX 읽기 시간 한도는 두 가지입니다.
  - `KAX_READ_TIMEOUT_SECS`(기본 8초, 1~120): 읽기가 **멈춘 채로** 이 시간이 지나면 `accessibility_read_limit`으로 중단합니다. 행을 하나 읽을 때마다 다시 잽니다. 행이 많아도 꾸준히 읽히는 동안에는 끊기지 않고, 앱이 응답하지 않을 때만 끊깁니다.
  - `KAX_CALL_TIMEOUT_SECS`(기본 120초, 5~600): `read_messages`·`load_more`·`search_messages` **도구 호출 한 번 전체**가 쓸 수 있는 시간입니다. 넘으면 `tool_call_time_limit`으로 중단하고, 이전 대화 로드는 `loadStopReason=tool_call_time_limit`으로 멈춥니다. `read_many`는 방마다 따로 적용합니다.
- 이전 대화 로드는 단계마다 대화 전체를 다시 읽지 않고 `AXRows` 개수로 늘었는지 판단하며, 새 행이 없으면 `loadStopReason=no_new_rows`로 멈춥니다(대화의 끝이라는 뜻은 아닙니다). 전체 읽기는 마지막에 한 번만 합니다.
- `read_messages` 스냅샷은 최신 1000행까지입니다. `search_messages`는 최신 5000행까지 한 번에 읽어 검색하고, 그보다 오래된 노출 행이 있으면 `unsearchedRowRange`(`start`·`end`·`count`)로 검색하지 않은 범위를 알립니다. 이전 대화를 불러왔다면 단계별 행 수를 `historyLoad`로 돌려줍니다.
- macOS 카카오톡은 화면 밖 행의 접근성 요청마다 셀을 새로 만들기 때문에, 화면 밖 행은 한 행에 수십 ms가 걸립니다(화면에 보이는 행은 1ms 미만). 그래서 수천 행을 읽으려면 수십 초가 걸릴 수 있습니다.

## 개발 및 검증

```bash
cargo test
```

단위 테스트에는 로그인된 앱이 필요하지 않습니다. 모의 Win32·클립보드와 합성 접근성 트리, 저널 이전 테스트로 양쪽 OS의 동작을 검증합니다.

## 라이선스

[MIT](LICENSE). 카카오톡 이용약관을 지키는지, 다른 사람의 대화를 어떻게 다루는지는 쓰는 사람의 책임입니다.
