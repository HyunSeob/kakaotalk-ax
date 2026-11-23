# kakaotalk-ax

macOS와 Windows의 공식 카카오톡 앱에서 채팅방 탐색, 본문·사진 읽기, 검색, 메시지 전송을 제공하는 CLI·Python 헬퍼·MCP 서버입니다.
같은 `kax` 명령과 `kax mcp` 서버가 macOS에서는 Swift Accessibility(AX), Windows에서는 내장 Win32 API를 사용합니다.
카카오톡 DB·프로세스 메모리·네트워크 데이터에는 접근하지 않습니다.

## 설치

### macOS

macOS, Python 3.10 이상, 로그인된 공식 카카오톡 앱, Swift 컴파일러가 필요합니다.
macOS 설정에서 실행하는 터미널 또는 에이전트에 손쉬운 사용 권한을 부여해야 합니다.

```bash
xcode-select --install
python3 -m venv .venv
.venv/bin/pip install -e '.[mcp,dev]'
.venv/bin/kax build --kind read
.venv/bin/kax build --kind control
.venv/bin/kax build --kind send
.venv/bin/kax status
```

네이티브 브리지는 소스 해시에 따라 로컬 캐시에 컴파일합니다. macOS의 MCP 서버에는 `mcp>=1.30,<2`가 필요합니다.

### Windows

Windows 10 또는 11, Python 3.10 이상, 같은 데스크톱 세션에서 실행 중인 공식 카카오톡 PC가 필요합니다.
방 목록과 사진 OCR에는 Windows 설정에서 **한국어 언어팩의 광학 문자 인식**을 설치합니다.
Windows 백엔드와 MCP stdio는 Python 표준 라이브러리와 Windows 내장 API로 실행합니다.

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -e .
.\.venv\Scripts\kax.exe build
.\.venv\Scripts\kax.exe status
```

Windows의 `build`는 앱 실행과 한국어 OCR 사용 가능 여부를 점검합니다. `status`는 양쪽 OS에서 열린 창을 조회합니다.

## Python API

```python
from kakaotalk_automation import NativeAX

ax = NativeAX()
windows = ax.windows()
rooms = ax.rooms()
found = ax.search_rooms("방 이름")
opened = ax.open_room("정확한 방 이름")
page = ax.read_messages("정확한 방 이름", limit=50)
batch = ax.read_many(["방 A", "방 B"], close_after=True)
older = ax.load_more("정확한 방 이름", pages=1)
matching = ax.search_messages("정확한 방 이름", "검색어")
ax.close_search("정확한 방 이름")
ax.close_room("정확한 방 이름")
```

`windows`는 창을 활성화하지 않고 열린 창 제목을 조회합니다. `rooms`와 `search_rooms`는 채팅 탭을 사용할 수 있으며 방을 열지 않습니다.
Windows 본문 읽기는 잠시 포커스와 클립보드를 사용합니다. 활성 창과 복원 가능한 클립보드 포맷을 복원하고, 사용자 입력이 계속되면 `user_input_active`로 중단합니다.
`read_many`는 macOS에서 최대 4개 프로세스로 본문을 병렬 조회하고 Windows에서 순차 조회합니다.
`close_after=True`는 해당 호출이 새로 연 창만 닫으며 기존 창과 초안은 보존합니다.
닫힌 방을 열면 카카오톡에서 읽음으로 표시할 수 있습니다. 열린 창만 읽으려면 `open_if_needed=False`를 사용합니다.

자세한 인수와 도구 대응은 [AX API](skills/kakaotalk-ax/references/ax-api.md)를 참고하세요.

## 메시지 전송

명시적으로 요청받은 방과 본문에만 전송하세요. 요청별 `operation_id`를 한 번 정하고 보관합니다.

```python
result = ax.send_message(
    window_title="정확한 방 이름",
    text="요청받은 메시지",
    operation_id="request-unique-id",
    require_self=False,
)
# 결과가 불확실하면 원래 ID로 조회만 합니다.
result = ax.send_result("request-unique-id")
```

Windows에서 전송할 때는 이미 열린 창의 `GetWindowTextW` 원문 제목을 사용합니다. macOS는 전송 전에 방을 열 수 있습니다.
본인 채팅은 `open_room(window_title, require_self=True)`로 열고, 전송에도 `require_self=True`를 지정합니다.
동명 방·기존 초안·확인할 수 없는 대상은 중단합니다. Windows는 로그인·잠금·모달 상태도 확인합니다.
전송 요청은 본문 해시와 함께 본인 전용 파일에 먼저 기록합니다. Windows 저널은 `%LOCALAPPDATA%\kakaotalk-ax\ax-send-operations`에 저장합니다. 같은 ID는 다시 보내지 않으며,
같은 ID를 다른 방이나 본문에 재사용하면 거부합니다.

`observed_in_ax`는 새 본문과 빈 입력창을 관찰했다는 뜻입니다. Windows 확인의 `confirmationSource`는 `windows_clipboard`입니다.
작성자·서버 ACK·상대방 읽음을 확인한 결과는 아닙니다. `outcome_unknown`, 응답 유실, 타임아웃에서는
새 ID로 재발송하지 말고 원래 ID로 `send_result`만 조회하세요. 불확실한 전송을 자동 재시도하지 않습니다.
파일 전송·메시지 수정·삭제는 제공하지 않습니다.

## CLI

```bash
.venv/bin/kax ax-windows
.venv/bin/kax ax-read --window-title '정확한 방 이름' --limit 50
.venv/bin/kax ax-call read_many --args-file /absolute/path/arguments.json
```

Windows에서는 위 명령의 실행 파일을 `.\.venv\Scripts\kax.exe`로 바꿉니다.
`ax-call`은 Python 메서드와 같은 이름과 UTF-8 JSON 인수를 사용합니다. 사용자 텍스트는 셸 코드 대신 JSON 파일로
전달하세요.
전송은 `ax-call send_message`, 결과 확인은 `ax-call send_result`입니다.

## MCP / 스킬

macOS MCP 설정 예시입니다. Windows에서는 `command`를 체크아웃의 `.venv\Scripts\kax.exe` 절대 경로로 지정합니다.

```json
{
  "mcpServers": {
    "kakaotalk-ax": {
      "command": "/absolute/path/kakaotalk-ax/.venv/bin/kax",
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

총 13개 AX 도구를 제공합니다.

저장소의 [카카오톡 스킬](skills/kakaotalk-ax/SKILL.md)은 이 AX 헬퍼를 사용합니다.
연결된 MCP가 없으면 스킬 클라이언트가 로컬 MCP 서버를 일회성으로 실행합니다.

```bash
.venv/bin/python skills/kakaotalk-ax/scripts/kax.py status
.venv/bin/python skills/kakaotalk-ax/scripts/kax.py tools
.venv/bin/python skills/kakaotalk-ax/scripts/kax.py call kakaotalk_ax_read --args-file /absolute/path/arguments.json
```

Windows 스킬 실행에는 `.\.venv\Scripts\python.exe`를 사용합니다. 별도 체크아웃은 `--root` 또는 `KAKAOTALK_AX_ROOT`로 지정합니다.
스킬은 저장소에 포함되며 자동으로 개인 스킬 폴더에 설치하지 않습니다.

## 읽기 범위와 제한

- `rooms`는 현재 보이는 행을 읽습니다. Windows 목록의 방 이름·미리보기는 `source=windows_ocr`, `textExact=false`이며 안 읽음 표시는 빨간 배지 픽셀을 판독합니다. 전송·읽기에는 열린 창의 원문 제목을 사용합니다. Windows는 목록 행 수를 제공하지 않아 `reportedRowCount=null`입니다.
- `read_messages`는 열린 창의 macOS AX 노출 행 또는 Windows 클립보드 원문을 최대 1000행 반환합니다. `limit`은 1~500입니다.
  읽은 범위는 `coverage`에 표시하며, 계정 전체·기간 전체의 기록을 보장하지 않습니다.
- `hasMore`는 수집한 스냅샷 안의 다음 페이지입니다. `nextOffset`과 `snapshotId`를 다음 호출에 전달하고,
  `snapshot_changed`면 첫 페이지부터 다시 읽으세요. `hasMore=false`도 과거 전체 기록의 끝을 뜻하지 않습니다.
- `kind=attachment`와 `attachments`는 첨부 표시를 보존합니다. Windows의 `sender`·`timeText`·`dateText`는 복사본에 포함된 문자열입니다. 인식되지 않은 시스템 행도 원문을 보존합니다.
- `load_more`는 대화창을 위로 스크롤해 이전 대화를 요청합니다. 실제 로딩량은 앱이 결정하며 `historyComplete=false`,
  `endOfHistory=null`입니다. 본문 검색은 이전 결과 버튼을 최대 `max_steps`번 순회하고 각 시점에 읽힌 본문과 주변 대화를 반환합니다.
- 여러 방 조회는 같은 시점의 원자적 스냅샷이 아닙니다. 방별 실패와 `partial`을 확인해야 합니다.
- Windows에서 앱이 최소화되거나 다른 창 뒤에 있으면 본문 읽기 동안 잠시 활성화될 수 있습니다. 안전하게 활성화할 수 없거나 복원할 수 없는 클립보드 포맷이 있으면 중단합니다. Windows의 캡처·내부 검색은 앱 창 구조와 표시 설정에 의존합니다.

## 검증

단위 테스트에는 로그인된 앱이 필요하지 않습니다. macOS의 Swift 판정 테스트는 컴파일러를 사용할 수 있을 때 합성 AX 요소로 실행합니다. Windows 테스트는 내장 API와 클립보드·OCR 모형을 사용하며 Swift 테스트를 건너뜁니다.

```bash
.venv/bin/python -m unittest discover -s tests -v
```

```powershell
.\.venv\Scripts\python.exe -m unittest discover -s tests -v
```

## 이미지 첨부와 검색 결과

```python
page = ax.read_messages("정확한 방 이름", limit=100)
photo = next(row for row in page["items"] if any(a["type"] == "image" for a in row["attachments"]))
image = ax.read_attachment("정확한 방 이름", photo["rowIndex"], page["snapshotId"])
print(image["imagePath"])  # 직접 볼 수 있는 PNG 이미지. 기본값은 OCR 없음.
# 필요한 경우에만 ocr=True를 전달합니다. 결과는 별도의 ocr 필드에 들어갑니다.

results = ax.search_messages("정확한 방 이름", "검색어", max_steps=10, context_rows=4)
```

macOS의 이미지 읽기는 macOS 14.2 이상과 화면 기록 권한이 필요합니다. Windows는 사진의 표시 위치를
클립보드 행으로 확인한 후 `PrintWindow`로 사진 영역을 캡처합니다. 위치가 모호하면 중단합니다.
이미지는 창의 표시 해상도로 반환하며 스크롤은 가능한 경우 복원합니다. `output_path`를 생략하면
임시 PNG를 저장합니다. 최신 스냅샷과 일치하지 않으면 먼저 다시 읽으세요.

검색은 `searchVerified`와 `stopReason`을 확인하세요. 검색 입력 후 실제 일치 본문이 보이고
상태가 안정된 경우만 검증된 검색으로 반환합니다. 이전 결과 이동이 확인되지 않으면
이미 얻은 항목을 보존하면서 `partial`로 반환합니다. 결과가 없다는 것을 확인하지 못한
경우도 성공으로 단정하지 않습니다. `max_steps`는 1..50, `context_rows`는 0..10입니다.
검색 도구의 네이티브 이전/다음 버튼 식별은 앱에 따라 AX 또는 Win32 검색 바 구조에 의존하며,
앱 업데이트로 구조가 바뀌면 탐색을 중단할 수 있습니다.
`navigation_exhausted`도 앱이 노출한 검색 탐색의 끝이며 계정 전체 기록의 완전성을 보장하지 않습니다.
각 항목에 `snapshotId`와 `searchVisit`을 붙이며, 서로 겹치는 스냅샷은 같은 메시지를
다시 포함할 수 있습니다(`duplicatesPossible`). 동일 본문의 서로 다른 메시지를 임의로 합치지 않습니다.
`total`은 수집한 일치 항목 수이며 고유 메시지 총수가 아닙니다.

## 라이선스

[MIT](LICENSE). 카카오톡 이용약관을 지키는지, 다른 사람의 대화를 어떻게 다루는지는 쓰는 사람의 책임입니다.
