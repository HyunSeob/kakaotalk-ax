# 카카오톡 네이티브 헬퍼 API

전체 API는 아래 13개 AX 도구다.

Python 진입점은 `from kakaotalk_automation import NativeAX; ax = NativeAX()`다.
macOS는 패키지에 포함된 Swift AX 브리지를 사용한다. Windows는 Win32·클립보드·내장 OCR을 사용한다.

| Python 메서드 | MCP 도구 |
|---|---|
| `windows()` | `kakaotalk_ax_windows` |
| `rooms(query="")` | `kakaotalk_ax_rooms` |
| `search_rooms(query)` | `kakaotalk_ax_search_rooms` |
| `open_room(window_title, require_self=False)` | `kakaotalk_ax_open` |
| `close_room(window_title)` | `kakaotalk_ax_close` |
| `read_messages(window_title, limit=50, offset=0, snapshot_id="")` | `kakaotalk_ax_read` |
| `read_attachment(window_title, row_index, snapshot_id, output_path=None, ocr=False)` | `kakaotalk_ax_read_attachment` |
| `read_many(window_titles, limit=50, open_if_needed=True, close_after=False, max_workers=3)` | `kakaotalk_ax_read_many` |
| `load_more(window_title, pages=1)` | `kakaotalk_ax_load_more` |
| `search_messages(window_title, query, limit=100, load_pages=0, case_sensitive=False, use_app_search=True, max_steps=10, context_rows=2)` | `kakaotalk_ax_search` |
| `close_search(window_title)` | `kakaotalk_ax_close_search` |
| `send_message(window_title, text, operation_id, wait_seconds=3, require_self=False)` | `kakaotalk_ax_send` |
| `send_result(operation_id, wait_seconds=0)` | `kakaotalk_ax_send_result` |

CLI는 macOS에서 `.venv/bin/kax ax-call METHOD --args-file FILE`, Windows에서 `.venv\Scripts\kax.exe ax-call METHOD --args-file FILE`이다.
인수 파일은 Python 메서드와 같은 키의 JSON 객체이며 `windows`/`rooms`는 생략할 수 있다.
스킬 클라이언트로는 `scripts/kax.py call MCP_TOOL --args-file FILE`을 사용한다.

일괄 조회 인수 예시

```json
{"window_titles":["방 A","방 B"],"limit":50,"close_after":true}
```

`read_many`는 일부 방이 실패해도 `items`에 방별 결과를 보존한다. `partial`이면 `reason`을 확인한다.
`close_after`는 그 호출이 새로 연 창에만 적용한다. 원래 열린 창과 작성 중인 초안은 보존한다. Windows는 `parallel=false`로 응답한다.

`rooms`/`search_rooms`의 `items`는 현재 보이는 방이다. macOS AX 테이블의 행 수는 `reportedRowCount`다. Windows는 이 값을 `null`로 반환한다.
두 값을 전체 방 목록으로 혼동하지 않는다. 앱 검색은 참여자 이름으로도 일치할 수 있다.
macOS의 `open_room` 일반 조회는 방 이름만 읽고 `require_self=True`는 자기 방 배지를 확인한다. 목록에서 방을 찾지 못했을 때의 `rooms`/`search_rooms` 폴백은
미리보기 등 기존 메타데이터를 계속 읽는다. 메뉴 열기는 표시 제목 대신 `openChatRoom:`
식별자를 사용하며, 활성 `AXMenuItem`이 정확히 하나일 때만 누른다.
Windows `rooms`와 `search_rooms`는 `PrintWindow`와 한국어 내장 OCR로 방 이름과 미리보기를 읽는다.
각 항목은 `rowIndex`, `textExact=false`, `source=windows_ocr`, 픽셀 기반 `unread`·`isSelf`('나' 원형 배지 감지 시 `true`)를 제공한다.
두 도구는 채팅방을 열지 않는다. 일반 `open_room`은 검색 후보를 좁힌 뒤 `GetWindowTextW`로
실제 제목을 확인하고 기존 방 검색어를 복원한다. `require_self=True`는 공식 앱 내 프로필의
‘나와의 채팅’ 동작으로 확인하며, Windows에서 본인 프로필명을 모를 때는 `open_room("나와의 채팅", require_self=True)`(또는 `"나"`)로 열어 실제 원문 `windowTitle`을 반환받을 수 있다(`send_message`는 항상 정확한 원문 제목만 허용). `windows`는 원문 제목과 `minimized`를 반환한다.

조회 한도 `limit`은 1~500행이다. `items`에는 본문 없는 행도 포함되므로 메시지 개수와 다르다.
한 번 수집하는 최대 범위는 1000행이다. 페이지는 `nextOffset`/`snapshotId`를 다음 호출의
`offset`/`snapshot_id`로 전달한다. `snapshot_changed`면 첫 페이지부터 다시 읽는다.
Windows에서 `source=windows_clipboard`는 `CF_UNICODETEXT` 원문이다. 그 안의 작성자·시각·날짜를
각각 `sender`·`timeText`·`dateText`에 보존한다. 복사 전 사용자 입력 상태를 확인하고 포커스와
클립보드 포맷을 복원한다. 새 클립보드 변경이 감지되면 그 내용을 보존하고 읽기를 중단한다.

`load_more.pages`는 1~10, `search_messages.load_pages`는 0~10이다. `endOfHistory=null`,
`coverage.historyComplete=false`는 의도된 한계다. 스크롤이나 검색이 과거 전체의 수집을 보장하지 않는다.
앱 검색은 방문한 시점에 읽힌 본문을 반환한다. Windows에서 포커스와 클립보드를 사용하는 작업은 프로세스 사이에서도 직렬화한다.

전송은 명시적으로 요청받은 방·본문에만 수행한다. `operation_id`는 1~128자의 영문·숫자·`_.:-`다.
`observed_in_ax`는 텍스트 행 증가와 빈 입력창의 관찰이며 작성자·서버 ACK·상대방 읽음을 보장하지 않는다.
응답 유실이나 `outcome_unknown`은 같은 ID로 결과만 확인한다. 다른 ID로 재발송하지 않는다.
초안·동명 방·확인할 수 없는 대상은 `blocked`다. `close_room`은 대화창만 닫는다.
Windows는 이미 열린 창의 원문 제목을 요구하고 미로그인·잠금·모달 상태를 차단한다.
`confirmationSource=windows_clipboard`는 저널 선기록 이후 새 본문과 빈 입력창을 확인한 결과다.

이미지는 `read_messages`의 `kind=attachment` 행과 최신 `snapshotId`를 `read_attachment`에 전달한다.
결과 `imagePath`를 직접 확인한다. 기본은 이미지 반환이며 OCR은 `ocr=True`일 때만 수행한다.
macOS는 14.2 이상과 화면 기록 권한이 필요하다. Windows는 선택한 행을 클립보드로 대조해
사진 영역을 확인하고 `PrintWindow`로 캡처한다. 위치가 모호하거나 렌더링이 바뀌면 중단한다.
창의 표시 해상도 PNG를 저장하며 스크롤은 가능한 경우 복원한다. 임시 PNG는 호출자가 사용 후 삭제한다.
검색은 이전 결과를 최대 `max_steps`(1..50)번 방문하며 `context_rows`(0..10)만큼 주변 행을 포함한다.
`searchVerified=false`/`partial`은 검색 동작의 결과가 확인되지 않았다는 뜻이다.
`stopReason`을 확인하고 방문 범위 밖의 결과를 없다고 판단하지 않는다.
중첩 스냅샷의 결과는 중복될 수 있으며 `total`은 고유 메시지 수가 아니다.
