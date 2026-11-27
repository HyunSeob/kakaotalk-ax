# 카카오톡 네이티브 헬퍼 API (`kax`)

전체 API는 아래 15개 AX 도구다.
단일 네이티브 바이너리 `kax`가 macOS에서는 Accessibility(AX)·ScreenCaptureKit·Vision OCR을, Windows에서는 Win32·클립보드·WinRT OCR을 직접 호출한다.

| `kax ax-call` 메서드 | MCP 도구 (`kax mcp`) |
|---|---|
| `windows` | `kakaotalk_ax_windows` |
| `rooms` (`query=""`, `scroll=false`, `limit=300`, `folder=null`) | `kakaotalk_ax_rooms` |
| `folders` | `kakaotalk_ax_folders` |
| `search_rooms` (`query`) | `kakaotalk_ax_search_rooms` |
| `open_room` (`window_title`, `require_self=false`) | `kakaotalk_ax_open` |
| `close_room` (`window_title`) | `kakaotalk_ax_close` |
| `members` (`window_title`, `limit=1000`) | `kakaotalk_ax_members` |
| `read_messages` (`window_title`, `limit=50`, `offset=0`, `snapshot_id=""`) | `kakaotalk_ax_read` |
| `read_attachment` (`window_title`, `row_index`, `snapshot_id`, `output_path=null`, `ocr=false`) | `kakaotalk_ax_read_attachment` |
| `read_many` (`window_titles`, `limit=50`, `open_if_needed=true`, `close_after=false`, `max_workers=3`) | `kakaotalk_ax_read_many` |
| `load_more` (`window_title`, `pages=1`) | `kakaotalk_ax_load_more` |
| `search_messages` (`window_title`, `query`, `limit=100`, `load_pages=0`, `case_sensitive=false`, `use_app_search=true`, `max_steps=10`, `context_rows=2`) | `kakaotalk_ax_search` |
| `close_search` (`window_title`) | `kakaotalk_ax_close_search` |
| `send_message` (`window_title`, `text`, `operation_id`, `wait_seconds=3`, `require_self=false`) | `kakaotalk_ax_send` |
| `send_result` (`operation_id`, `wait_seconds=0`) | `kakaotalk_ax_send_result` |

CLI는 macOS·Windows 모두 `kax ax-call METHOD --args-file FILE` 형태를 사용한다.
인수 파일은 위 키의 UTF-8 JSON 객체이며 `windows`/`rooms`는 생략할 수 있다.

일괄 조회 인수 예시:

```json
{"window_titles":["방 A","방 B"],"limit":50,"close_after":true}
```

`read_many`는 일부 방이 실패해도 `items`에 방별 결과를 보존한다. `partial`이면 `reason`을 확인한다.
`close_after`는 그 호출이 새로 연 창에만 적용한다. 원래 열린 창과 작성 중인 초안은 보존한다. Windows는 `parallel=false`로 응답한다.

`rooms`/`search_rooms`의 `items`는 현재 보이는 방이다. macOS AX 테이블의 행 수는 `reportedRowCount`다. Windows는 이 값을 `null`로 반환한다.

macOS `folders`는 채팅 목록 위 "채팅방 폴더" 목록에서 `{name, index, unread, selected}`를 읽는다. `rooms(folder=이름)`은 폴더 목록의 `AXSelectedChildren`으로 그 폴더를 고르고(마우스 없음) 방 목록을 읽은 뒤(`scroll`과 함께 사용 가능) 원래 폴더로 되돌린다. 결과에 `folder`(`name`·`index`·`unread`), `folderChanged`, `folderRestored`가 붙는다. 방 검색어가 있으면 `room_search_active`, 이름이 없으면 `folder_not_found`, 같은 이름이 여럿이면 `room_folder_name_ambiguous`. `rooms` 항목에는 `unread`·`memberCount`·`lastMessageTime`·`muted`·`pinned`가 있다.

macOS에서 입력창(composer)이 없는 단방향 채널 창(알림톡 등)은 대화 표가 하나면 읽을 수 있다(`windows`의 `hasComposer=false`). `close_room`은 입력창이 0개면 초안 검사 없이 닫고, 2개 이상이면 `chat_composer_not_unique`로 막는다. 대화 표가 없으면 `channel_window_unreadable`, 전송은 `chat_composer_unavailable`이다. `open_room`이 쓸 수 없는 창을 열었으면 `windowOpened=true`를 돌려주고, `read_many`는 그런 창을 항상 다시 닫는다. 메인 창은 계속 `main_window_is_not_a_chat`이다.

macOS `members(window_title, limit=N)`은 열린 단체방 창의 `대화상대 N명` 버튼으로 목록 팝오버를 열고, 최대 N명(1~3000)을 읽은 뒤 `AXCancel`로 닫는다. 사용자가 이미 열어 둔 목록은 읽기만 하고 닫지 않는다(`listWasOpen`). 항목은 `name`·`rowIndex`·`isSelf`(`badge me`)·`role`(`badge openchat king`→`owner`, `badge openchat staff`→`staff`)·`badges`다. `memberCount`는 버튼의 숫자이고, `complete=true`는 그만큼 모두 읽었다는 뜻이다. `stopReason`은 `complete`·`limit`·`member_row_stalled`·`tool_call_time_limit`이고, 읽지 못한 행 수는 `skippedRows`다. 목록 행 자체를 읽지 못하면 `member_rows_unavailable`이다. 버튼이 없으면(1:1방·나와의 채팅) `member_list_unavailable`, Windows는 `member_list_unsupported`.

macOS `rooms(scroll=true, limit=N)`은 채팅 목록을 맨 위부터 스크롤하며 `rowIndex` 기준으로 최대 N개(1~1000)를 모으고 원래 스크롤 위치로 되돌린다(`scope=scrolled_room_list`). `stopReason`은 `complete`·`limit`·`end_of_list`·`no_new_rows`·`user_scrolled`·`tool_call_time_limit` 중 하나이고, `historyComplete=true`는 `reportedRowCount`개를 모두 읽었고 도중에 목록이 바뀌지 않았다(`listChanged=false`)는 뜻이다. 사용자가 도중에 목록을 스크롤하면 멈추고 위치를 되돌리지 않는다(`userScrolled=true`). Windows는 `room_list_scroll_unsupported`로 막는다.
macOS에서 `require_self=true`는 본인 배지가 있는 방을 목록에서 열어 실제 채팅 창을 확인한다. 목록에서 숨긴 자기 방은 친구 탭의 ‘내 기본프로필 → 나와의 채팅’으로 확인한다. 전송 직전에도 같은 창인지 검사한다.
Windows `rooms`와 `search_rooms`는 `PrintWindow`와 한국어 내장 OCR로 방 이름과 미리보기를 읽는다. 각 항목은 `rowIndex`, `textExact=false`, `source=windows_ocr`, 픽셀 기반 `unread`·`isSelf`를 제공한다.
`require_self=true`는 공식 앱 내 프로필의 ‘나와의 채팅’ 동작으로 확인하며, Windows에서 본인 프로필명을 모를 때는 `open_room("나와의 채팅", require_self=true)`로 열어 실제 원문 `windowTitle`을 반환받을 수 있다(`send_message`는 항상 정확한 원문 제목만 허용).

조회 한도 `limit`은 1~500행이다. 한 번 수집하는 최대 범위는 1000행이다. 페이지는 `nextOffset`/`snapshotId`를 다음 호출의 `offset`/`snapshot_id`로 전달한다. `snapshot_changed`면 첫 페이지부터 다시 읽는다.
macOS `read` 항목의 메시지 정보: `sender`는 그 행에 보이는 이름(남이 묶음을 시작한 행에만 있음)이다. 이름 없는 행은 앞뒤 맥락으로 판정한다. 묶음이 끝난 행(시각 있음)·날짜 구분선·시스템 알림 바로 뒤의 이름 없는 행은 말풍선이 오른쪽에 붙었는지(`AXFrame`)를 확인해 `isSelf: true`(`senderSource: self_layout`)로 보고, 앞 행에 시각이 없으면 앞 행의 보낸 사람을 이어받는다(`groupSender`, `senderSource: inherited`). 오른쪽에 붙지 않은 이름 없는 행은 남의 메시지지만 누가 보냈는지 모르므로 `groupSender: null`, `senderSource: not_own_layout`이다. 위치를 읽지 못하거나 맥락이 없으면 `null`이다. `groupTimeText`는 묶음 끝 행의 시각, `date`(ISO)·`dateText`는 위쪽 가장 가까운 날짜 구분선(`kind: date_separator`)에서 온다. `unreadCount`는 "안읽음 N", `edited`는 "수정됨" 표시, `truncated`는 카카오톡이 500자(UTF-16 단위)에서 잘라 보여 준 메시지다. 파일은 `kind: file`과 `file.name`·`size`·`expiresAt`, 가운데 정렬된 알림은 `kind: system`과 `systemType`(`join`·`leave`·`kick`·`invite`·`role`·`hidden`·`deleted`·`notice`)·`target`이다. `search_messages`(최대 5000행) 경로는 위치를 읽지 않아 `isSelf`가 `null`일 수 있다.
`load_more`는 기본으로 요약(`steps`의 행 수, `after`의 `snapshotId`·`total`·`oldestDate`·`newestDate`·`rowsTruncated`·`truncatedBefore`, `warning`, `itemsIncluded=false`)만 돌려준다. 본문은 `read(window_title, limit, offset, snapshot_id)`로 나눠 읽고, `include_items=true`면 예전 형식(`steps` 전체와 `after.items`)이다. `load_more.pages`는 1~10, `search_messages.load_pages`는 0~10이다. `endOfHistory=null`, `coverage.historyComplete=false`가 기본이다. macOS에서 맨 위 날짜 구분선 바로 다음 행이 오픈채팅 입장 안내(사칭 주의·불법촬영물 안내, `room_creation_notice`)나 팀채팅 시작 문구(`team_chat_start`)면 `read`·`load_more` 모두 `historyComplete=true`와 `historyCompleteReason`·`historyStartRowIndex`를 주고, `load_more`는 `loadStopReason: start_of_history`, `endOfHistory: true`로 바로 멈춘다. 일반 단체방의 초대 문구(`group_invite_at_top`)는 대화 중간에도 나오므로 `load_more`가 한 번 더 길게 기다려도 늘지 않을 때만 인정하고, `read`에는 `historyStartHint`로만 준다. 1000행 제한으로 잘린 `read`는 `false`다.

전송은 명시적으로 요청받은 방·본문에만 수행한다. `operation_id`는 1~128자의 영문·숫자·`_.:-`다.
`observed_in_ax`는 텍스트 행 증가와 빈 입력창의 관찰이며 작성자·서버 ACK·상대방 읽음을 보장하지 않는다.
macOS는 대화창 전체가 아니라 끝부분(전송 전 마지막 15행, 전송 후 그 시작부터 끝까지, 최대 80행)만 읽어 확인한다.
전송 전 행이 같은 순서로 남아 있고 행 수가 늘었으며 새 행 중 보낸 텍스트가 정확히 한 번이어야 하고, 다른 새 텍스트가 섞이면 보낸 행이 내 메시지 쪽(오른쪽 정렬)이어야 한다.
이모지 하나만 보내면 카카오톡이 큰 이미지로 그리므로, 내 쪽의 설명 없는 이미지 행 하나가 새로 생겼을 때 `reason: new_emoji_image_row_and_empty_composer`로 관찰한다(이모지 종류는 AX로 알 수 없다). 카카오톡은 500자(UTF-16 단위, 이모지는 2, 걸친 이모지는 통째로)를 넘는 메시지를 앞부분과 `…`로만 보여 주므로, 긴 텍스트는 그 모양과 맞는지로 확인한다.
응답 유실이나 `outcome_unknown`은 같은 ID로 결과만 확인한다. 다른 ID로 재발송하지 않는다.
구버전(Python)에서 기록된 전송 저널은 `kax migrate-journal`(또는 첫 전송/조회 시 자동)로 단방향 이전되며 같은 `operation_id`의 중복 전송 방지와 결과 조회를 이어간다.

이미지는 `read_messages`의 `kind=attachment` 행과 최신 `snapshotId`를 `read_attachment`에 전달한다.
결과 `imagePath`를 직접 확인한다. 기본은 이미지 반환이며 OCR은 `ocr=true`일 때만 수행한다.
검색은 이전 결과를 최대 `max_steps`(1..50)번 방문하며 `context_rows`(0..10)만큼 주변 행을 포함한다.
`searchVerified=false`/`partial`은 검색 동작의 결과가 확인되지 않았다는 뜻이다.
