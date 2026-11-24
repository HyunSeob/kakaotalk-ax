---
name: kakaotalk-ax
description: "카카오톡(카톡)의 방 열기·닫기·본문 읽기·일괄 읽기·검색·이전 대화 요청·메시지 전송을 단일 네이티브 kax CLI 및 MCP 서버로 수행한다."
---

# kakaotalk-ax

앱 자동화는 설치된 `kax` 단일 네이티브 바이너리와 `kakaotalk_ax_*` MCP 도구를 사용한다.
`kax`가 macOS의 Accessibility(AX)와 Windows의 Win32·WinRT 백엔드를 자동 선택한다.

## 실행

연결된 `kakaotalk_ax_*` MCP 도구(`kax mcp`)를 우선 사용한다.
MCP 연결이 없으면 설치된 `kax` CLI를 직접 호출한다. 사용자 텍스트는 셸 코드에 넣지 않고 UTF-8 JSON 파일로 전달한다.

```bash
kax status
kax ax-windows
kax ax-read --window-title '정확한 방 이름' --limit 50
kax ax-call rooms --args-file /absolute/path/arguments.json
kax ax-call send_message --args-file /absolute/path/request.json
```

Windows에서도 같은 명령(`kax.exe` 또는 `kax`)을 사용한다. 채팅방 목록 탐색에 필요한 한국어 OCR 언어팩을 Windows 설정에서 설치한다.
메서드·도구 대응과 정확한 인수는 [AX API](references/ax-api.md)를 읽는다.

## 카카오톡 자동화

- 열린 창은 `kakaotalk_ax_windows`(`kax ax-windows`), 방 목록은 `kakaotalk_ax_rooms`(`kax ax-call rooms`)로 확인한다.
  정확한 방 이름으로 `kakaotalk_ax_open`을 호출한다. 앱의 방 검색은 `kakaotalk_ax_search_rooms`이며 빈 검색어는 해제다.
  동명 방은 중단한다. Windows 목록의 `windows_ocr` 이름과 미리보기는 탐색용이다. 열린 창의 원문 제목을 확인하고 사용한다.
- 한 방은 `kakaotalk_ax_read`, 여러 방은 `kakaotalk_ax_read_many(window_titles=[...])`를 사용한다.
  일괄 도구가 창 열기와 읽기를 수행한다. macOS는 병렬, Windows는 포커스·클립보드 충돌 방지를 위해 순차 조회한다.
  `close_after=true`이면 그 호출이 새로 연 창만 닫는다. 창만 닫기는 `kakaotalk_ax_close`이며 초안은 보존한다.
- `items`에는 현재 앱에서 읽힌 행이 포함된다. `kind=text`의 `text`가 메시지 본문이고,
  이미지는 `attachment`/`attachments`로 보존한다. Windows는 클립보드에 포함된 작성자·날짜·시각을 제공한다. `user_input_active`는 사용자 조작과의 충돌을 뜻한다.
- `hasMore=true`면 `nextOffset`/`snapshotId`를 다음 호출의 `offset`/`snapshot_id`로 전달한다.
  `snapshot_changed`면 첫 페이지부터 다시 읽는다. `hasMore=false`라도 전체 과거 기록을 다 읽었다는 뜻은 아니다.
- 이전 대화는 `kakaotalk_ax_load_more(window_title, pages=1)`로 제한해서 요청한다. 추가 행이 없다는
  결과만으로 전체 과거 기록의 끝이라고 판단하지 않는다. 최대 1000행이며 `coverage`를 확인한다.
- `kakaotalk_ax_read_attachment`는 최신 `row_index`/`snapshot_id`로 사진 영역을 PNG `imagePath`에 반환한다.
  이미지를 직접 확인하고, 텍스트 추출이 필요한 경우에만 `ocr=true`를 쓴다. macOS에는 화면 기록 권한이 필요하다.
- `kakaotalk_ax_search(window_title, query)`는 앱 검색의 이전 결과를 제한된 횟수만큼 순회하며 본문과 주변 행을 반환한다.
  `searchVerified`/`stopReason`을 확인한다. 겹치는 스냅샷은 같은 메시지를 다시 포함할 수 있다.
  `use_app_search=false`는 노출 본문만 검사한다. `kakaotalk_ax_close_search`로 검색 UI를 닫는다.
- 전송은 `kakaotalk_ax_send(window_title, text, operation_id)`(`kax ax-call send_message`)다. 요청 ID를 한 번 정해서 보관한다.
  Windows는 이미 열린 창의 정확한 원문 제목을 요구한다. 본인 방을 열 때와 전송할 때 모두 `require_self=true`를 쓴다
  (Windows에서 본인 프로필명을 모르면 `kakaotalk_ax_open(window_title="나와의 채팅", require_self=true)`로 먼저 열어 반환된 원문 `windowTitle`을 사용한다).
  기존 초안이 있으면 중단한다. `observed_in_ax`는 새 텍스트와 빈 입력창을 관찰한 결과이며 작성자·서버 ACK·읽음 확인이 아니다.
  타임아웃·`outcome_unknown`에서는 원래 ID로 `kakaotalk_ax_send_result`만 조회하고 새 ID로 재발송하지 않는다.

## 제공 범위

`status`는 열린 창을 확인한다.
최근 일주일처럼 기간 전체를 요청받아도 앱에서 읽힌 범위를 명시한다.
이미지는 `read_attachment`로 실제 확인한 범위만 설명한다. 원본 첨부·작성자·생략된 시각·전체 기록을 확인했다고 추측하지 않는다.
전송 본문은 사용자의 요청을 따른다. 본인 방에 검증을 요청받아 문구를 정해야 한다면
자연스러운 짧은 문장을 사용하고, 사용자가 원하지 않은 기술 설명이나 표식을 넣지 않는다.
