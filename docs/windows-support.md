# Windows PC 카카오톡 지원 설계 기록 (Python 버전)

이 문서는 Rust 전환 전 Windows 백엔드의 설계와 실측 기록입니다. 현재 `kax.exe`의 설치와 실행 방법은 [README의 Windows 설치 안내](../README.md#windows-x64), 호출 인수는 [AX API](../skills/kakaotalk-ax/references/ax-api.md)를 참고하세요.

## 1. 배경 및 목표

- **배경**: `kakaotalk-ax`(`kax`) 공개 이후 업무 환경이 Windows인 실사용자들로부터 Windows PC 카카오톡 지원 요청이 다수 접수됨.
- **목표**: 단일 저장소·패키지(`kakaotalk-ax` / CLI `kax`)와 동일한 13개 Python API·MCP 도구 인터페이스(`kakaotalk_ax_*`)를 유지하면서 공식 Windows PC 카카오톡(`KakaoTalk.exe`) 지원을 추가함.
- **핵심 제품 원칙**:
  1. **원문 무결성 보장**: 대화 본문 읽기·검색·방 제목 확정·메시지 전송은 전부 **100% 정확한 원문 문자열(Win32 텍스트 API + 클립보드 `CF_UNICODETEXT`)** 만 사용하며 OCR 결과에 의존하지 않음.
  2. **실사용 탐색성 확보**: 접근성 노드와 복사가 모두 막혀 있는 메인 창의 닫힌 채팅방 목록(`rooms`, `search_rooms`)은 방을 열어 '안 읽음(`1`)'을 깨뜨리지 않도록 **비침투 캡처(`PrintWindow`) + 내장 한국어 OCR + 안 읽음 배지 픽셀 감지**를 탐색 보조 수단으로 제공함.
  3. **계정 및 사용자 상태 안전성**: 로컬 DB·프로세스 메모리·패킷에 일절 접근하지 않으며(`databaseCalls: 0`), 작성 중인 초안·포커스·클립보드를 보존함.

---

## 2. 비목표 및 절대 금지선 (Non-Goals & Hard Constraints)

- **앱 내부 데이터 비접근**: 로컬 암호화 DB 열람/복호화, `ReadProcessMemory`, DLL 인젝션, GDI/DirectWrite 텍스트 드로잉 후킹, 네트워크 패킷 가로채기 전면 금지.
- **외부 런타임 의존성 추가 금지**: `pywin32`, `uiautomation`, `pytesseract` 등 서드파티 Python 패키지나 별도 C++/C# 컴파일러 설치를 요구하지 않음. Python 3.10+ 표준 라이브러리(`ctypes`)와 Windows 10/11 내장 구성요소(Win32 API, WinRT `Windows.Media.Ocr`)만으로 동작해야 함.
- **파괴적 기능 미제공**: macOS 버전과 동일하게 파일 전송, 메시지 수정·삭제, 채팅방 나가기(`채팅방 나가기` 메뉴 호출)는 제공하지 않음.

---

## 3. 실기기 실측 결과 (`KakaoTalk 26.10.0.5364` x64 / Windows 11 `10.0.26200`)

실제 로그인된 Windows 11 환경(`C:\Program Files\Kakao\KakaoTalk\KakaoTalk.exe`, 64-bit)에서 검증한 기술적 사실은 다음과 같음.

### 3.1 접근성 트리 (UIA / MSAA) 노출 여부
- **UI Automation (`IUIAutomation`)**: 메인 창 전체 자손 노드가 레이아웃 컨테이너(`ControlType.Pane`) 29개뿐이며, 방 이름·미리보기·대화 말풍선 텍스트 노드는 0개임.
- **MSAA (`IAccessible` / `WM_GETOBJECT`)**: Windows 스크린리더 플래그(`SPI_SETSCREENREADER = TRUE`)를 켠 상태에서도 커스텀 리스트 `EVA_VH_ListControl_Dblclk`은 `WM_GETOBJECT`에 `0`(미구현)을 반환하며, 기본 클라이언트 프록시(`ROLE_SYSTEM_CLIENT`) 외 항목 텍스트를 전혀 노출하지 않음.
- **기타 우회 탐색 검증**:
  - 리스트 행 호버 시 `tooltips_class32` 텍스트 미노출, `F2` 단축키 무반응.
  - 우클릭 컨텍스트 메뉴(`EVA_Menu`)의 `채팅방 이름 설정` 다이얼로그 내부 `Edit`(`ctrlId=100`) 역시 사용자가 수동 별칭을 지정하지 않은 기본 방에서는 빈 문자열(`len=0`)이며 플레이스홀더마저 커스텀 드로잉으로 그림.

### 3.2 Win32 윈도우 및 컨트롤 계층 구조
- **메인 창**: 클래스 `EVA_Window_Dblclk`, 제목 `'카카오톡'`
  - `OnlineMainView_*` (`EVA_ChildWindow`, `ctrlId=132`): 로그인 완료 시 활성화되는 메인 컨테이너.
  - `LockModeView_*` (`EVA_ChildWindow_Dblclk`, `ctrlId=183`): 잠금모드 컨테이너 (`IsWindowVisible`로 잠금 상태 즉시 판별 가능).
  - 탭 뷰: 친구 탭 `ContactListView_*`(`ctrlId=1149`), 채팅 탭 `ChatRoomListView_*`(`ctrlId=1150`), 더보기 탭 `MoreView_*`(`ctrlId=1075`). 좌측 네비게이션 영역 `PostMessage` 클릭으로 포커스 침탈 없이 백그라운드 탭 전환 가능 확인.
  - 채팅 탭(`ChatRoomListView_*`) 내부 자식 컨트롤:
    - `ChatRoomListCtrl_*` (`EVA_VH_ListControl_Dblclk`, `ctrlId=1003`): 기본 채팅방 목록.
    - `SearchListCtrl_*` (`EVA_VH_ListControl_Dblclk`, `ctrlId=1003`): 방 검색 시 표시되는 결과 목록 (`IsWindowVisible`로 검색 활성 상태 판별).
    - 방 검색 입력창 (`Edit`, `ctrlId=100`): 표준 Win32 `Edit` 컨트롤로 `WM_GETTEXT` / `WM_SETTEXT`를 통해 기존 검색어 조회 및 복원 가능.
    - `_EVA_CustomScrollCtrl` (`ctrlId=202`): 커스텀 스크롤바.
- **열린 대화창**:
  - 최상위 창 핸들(`#32770` 또는 `EVA_Window_Dblclk`)의 `GetWindowTextW`가 **100% 정확한 원문 방 제목**을 반환함.
  - 본문 리스트(`EVA_VH_ListControl_Dblclk`)는 포커스 상태에서 `Ctrl+A` → `Ctrl+C` 실행 시 `CF_UNICODETEXT` 포맷으로 날짜 구분선 및 `[작성자] [오전/오후 H:MM] 본문` 원문을 복사함.
  - 메시지 입력창(`RICHEDIT50W`)은 비침투 Win32 메시지(`WM_GETTEXTLENGTH`, `WM_GETTEXT`)로 기존 초안 유무를 원문 그대로 확인할 수 있음.
- **비침투 화면 캡처**:
  - 창이 다른 창 뒤에 있거나 비포커스 상태일 때도 `PrintWindow(hwnd, hdc, PW_RENDERFULLCONTENT=2)`로 검은 영역 없이 정상 비트맵 캡처가 가능함을 확인함.
  - Windows 내장 `Windows.Media.Ocr`(`ko`, `en-US`)가 기본 설치되어 있어 외부 OCR 엔진 없이 로컬 인식 가능.

---

## 4. 패키지 및 아키텍처 통합 설계

- **기준 브랜치**: `origin/main` 최신 커밋(`da3ebba`, 이미지 첨부 `read_attachment` 및 페이지 순회형 `search_messages`가 포함된 13개 도구 기준)으로 fast-forward 후 작업.
- **단일 패키지 OS 자동 분기**:
  - 패키지명(`kakaotalk-ax`), CLI 명령(`kax`), Python 진입점(`from kakaotalk_automation import NativeAX`), MCP 도구명(`kakaotalk_ax_*` 13종), 스킬 스크립트(`skills/kakaotalk-ax/scripts/kax.py`)를 동일하게 유지함.
  - `sys.platform == "darwin"`: 기존 Swift AX 브리지(`src/kakaotalk_native/*.swift`) 사용.
  - `sys.platform == "win32"`: 순수 Python `ctypes` 기반 Windows 백엔드 모듈(예: `src/kakaotalk_win32.py`)로 분기. Windows에서 `kax build`는 컴파일 없이 환경 점검(`KakaoTalk` 실행 여부·WinRT OCR 가용성 확인) 후 성공 반환.
- **크로스플랫폼 파일 락 및 상태 저널**:
  - `kakaotalk_ax.py`의 `ui_session()`과 `kakaotalk_state.py`의 `send_lock()`·`save_private()`에서 POSIX 전용 호출(`fcntl.flock`, `os.O_NOFOLLOW`, 디렉터리 `os.fsync`)을 플랫폼 분기 처리함.
  - Windows에서는 `msvcrt.locking`(`LK_NBLCK`) 또는 `kernel32.LockFileEx`(`LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`)로 프로세스 간 배타 락을 구현하고, `MoveFileExW(..., MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`(`os.replace` + 파일 `fsync`)로 원자적 저널 기록을 보장함.
- **응답 메타데이터 규칙**:
  - `source` 필드에 실제 데이터 원천을 구분 표기함:
    - Win32 창/컨트롤 메타데이터: `"windows_win32"`
    - 클립보드 기반 원문 읽기: `"windows_clipboard"`
    - 화면 캡처 + 내장 OCR 기반 목록 탐색: `"windows_ocr"`
  - MCP 클라이언트·스킬 호환을 위해 최상위 상태 코드(`ok`, `partial`, `snapshot_changed`, `observed_in_ax`, `outcome_unknown`, `blocked`)는 기존 계약을 유지하되 `confirmationSource` 등에 플랫폼 원천을 명시함.

---

## 5. 13개 도구별 상세 요구사항

| # | Python 메서드 | MCP 도구 | 원문 보장 | Windows 동작 요구사항 |
|---|---|---|---|---|
| 1 | `windows()` | `kakaotalk_ax_windows` | 100% 원문 | `EnumWindows`로 `KakaoTalk.exe`의 열린 대화창 제목(`GetWindowTextW`), 최소화 여부(`IsIconic`), 읽기 가능 여부를 비침투 조회. 포커스·초안 무개입. |
| 2 | `rooms(query="")` | `kakaotalk_ax_rooms` | 탐색용 OCR (`textExact: false`) | 백그라운드로 채팅 탭(`ChatRoomListView`) 활성화 후 `ChatRoomListCtrl`을 `PrintWindow` 캡처. 행 단위로 분할·확대하여 내장 한국어 OCR로 방 이름·미리보기 추정치 추출 및 빨간 배지 픽셀로 `unread` 여부 판별. **방을 열지 않으므로 읽음(`1` 소멸) 부작용 없음.** 각 항목에 `rowIndex`와 `textExact: false` 포함. |
| 3 | `search_rooms(query)` | `kakaotalk_ax_search_rooms` | 입력 100% 원문 / 결과 탐색용 OCR | 채팅 탭 검색 `Edit`(`ctrlId=100`)에 `query` 설정(빈 문자열은 검색 해제) 후 `SearchListCtrl`을 `PrintWindow` + 내장 OCR로 읽어 반환. 앱 내부 검색 필터 자체는 100% 원문(방 이름·참여자명) 기준으로 동작함. |
| 4 | `open_room(window_title, require_self=False)` | `kakaotalk_ax_open` | **100% 원문 확정** | ① 이미 열린 창 중 제목 일치 시 재오픈 없이 반환(동명 열린 창 복수 시 차단). ② 닫혀 있으면 기존 검색어를 백업한 뒤 검색 `Edit`에 `window_title`을 입력해 열고, **실제 열린 대화창의 `GetWindowTextW` 원문 제목**을 확인·반환한 뒤 검색어를 복원함(`rooms`의 미세 OCR 오차가 있어도 카톡 검색 매칭 시 열린 창의 실제 원문 제목으로 확정하되, 복수 후보가 모호하거나 제목 불일치 시 안전하게 중단/구분). |
| 5 | `close_room(window_title)` | `kakaotalk_ax_close` | 100% 원문 | 정확한 제목의 열린 대화창에서 `RICHEDIT50W`에 작성 중인 초안이 없을 때만 `WM_CLOSE`로 창만 닫음(초안 존재 시 `existing_draft_preserved` 차단). |
| 6 | `read_messages(window_title, limit=50, offset=0, snapshot_id="")` | `kakaotalk_ax_read` | **100% 원문** | 열린 대화창의 메시지 리스트에 짧은 포커스 후 `Ctrl+A` → `Ctrl+C`(포커스·클립보드 즉시 복원). 복사된 UTF-16 텍스트에서 날짜 구분선과 `[작성자] [오전/오후 H:MM] 본문`(멀티라인 포함)을 파싱하여 `sender`, `timeText`, `dateText`, `text`를 원문 그대로 구성. `snapshotId` 기반 오프셋 일관성(`snapshot_changed`) 동일 지원. |
| 7 | `read_many(window_titles, ...)` | `kakaotalk_ax_read_many` | **100% 원문** | Windows에서는 포커스·클립보드가 전역 공유 자원이므로 내부 읽기를 `ui_session` 아래 안전하게 직렬 수행(`parallel: false`). 방별 부분 실패 보존 및 `close_after=True` 시 해당 호출이 새로 연 창만 닫기 유지. |
| 8 | `load_more(window_title, pages=1)` | `kakaotalk_ax_load_more` | **100% 원문** | 열린 대화창 리스트에서 상단 스크롤(`VK_HOME` / `VK_PRIOR` / `WM_VSCROLL`)을 최대 `pages`(1..10)회 요청하며 전후 본문 스냅샷 변화를 비교. `historyComplete=false`, `endOfHistory=null` 유지. |
| 9 | `search_messages(...)` | `kakaotalk_ax_search` | **100% 원문** | `use_app_search=False`면 현재 읽힌 원문 행에서 문자 그대로 검색. `use_app_search=True`면 대화창 내 본문 검색(`Ctrl+F`) 바에 `query`를 입력하고 이전 결과 이동을 최대 `max_steps`(1..50)회 순회하며 각 지점의 클립보드 원문과 주변 `context_rows`행을 수집. |
| 10 | `close_search(window_title)` | `kakaotalk_ax_close_search` | 100% 원문 | 열린 대화창의 본문 검색 바만 닫고 `RICHEDIT50W`의 작성 중 초안은 보존함. |
| 11 | `read_attachment(window_title, row_index, snapshot_id, output_path=None, ocr=False)` | `kakaotalk_ax_read_attachment` | PNG 캡처 (선택적 OCR) | 최신 `snapshotId` 일치 검증 후 사진 메시지(`사진` / 이미지 행) 위치가 보이도록 스크롤하고 `PrintWindow`로 해당 대화창 사진 영역을 캡처해 PNG 반환(기본 `ocr=False`, `ocr=True`일 때만 `Windows.Media.Ocr` 텍스트를 별도 `ocr` 필드에 추가). 가능 시 스크롤 복원. |
| 12 | `send_message(window_title, text, operation_id, wait_seconds=3, require_self=False)` | `kakaotalk_ax_send` | **100% 원문 필수** | **열린 창의 `GetWindowTextW`가 `window_title`과 100% 글자 단위로 일치할 때만 허용**(근사 일치 절대 금지). 초안·모달·잠금모드·동명 창 검사 → 베이스라인 본문 해시 기록 → `operation_id` 저널 선기록 → `RICHEDIT50W` 원문 입력 및 단 1회 전송 → 새 본문 해시 출현 + 빈 입력창 관찰 시 `observed_in_ax`, 미확인 시 자동 재전송 없이 `outcome_unknown` 반환. |
| 13 | `send_result(operation_id, wait_seconds=0)` | `kakaotalk_ax_send_result` | **100% 원문** | 저장된 `operation_id` 저널을 조회하고 미확정 상태면 창을 새로 열거나 재전송하지 않고 읽기만으로 결과 재확인. |

---

## 6. UI 개입 보호 및 안전장치 기준

Windows 백엔드는 본문 복사(`Ctrl+A/C`)와 방 열기 과정에서 짧은 포커스·클립보드 개입이 수반되므로 아래 안전장치를 필수로 구현함.

1. **사용자 실시간 입력 충돌 방지 (`GetLastInputInfo`)**:
   - 포커스 전환이나 클립보드 조작 직전 Win32 `GetLastInputInfo`로 사용자의 마지막 키보드·마우스 입력 시각을 확인함.
   - 최근 유휴 시간(예: 400ms) 이내에 사용자가 직접 타이핑·조작 중이면 짧게 대기하며, 제한 시간 내 유휴 상태가 되지 않으면 사용자 작업을 끊지 않고 `blocked`(`reason: "user_input_active"`)로 안전 중단함.
2. **클립보드 다중 포맷 백업 및 보장 복원**:
   - 조작 전 클립보드의 모든 주요 포맷(`CF_UNICODETEXT`, `CF_DIB`, `CF_HDROP`, HTML/RTF 등 등록 포맷 바이트)을 메모리에 스냅샷 백업하고 `try ... finally` 블록에서 예외 발생 여부와 무관하게 원상 복원함.
   - `GetClipboardSequenceNumber` 변화로 실제 복사 완료 여부를 검증하여 이전 클립보드 잔류 데이터를 대화 본문으로 오인하는 사고를 차단함.
3. **활성 창 포커스 및 선택 영역 복원**:
   - 개입 직전 `GetForegroundWindow()` 핸들을 기록하고, 복사 직후 선택 하이라이트를 해제한 뒤 원래 활성 창 포커스를 복원함(원래 최소화되어 있던 카톡 창을 열었거나 건드린 경우 상태 보존).
4. **앱 상태 및 대상 모호성 차단**:
   - 카카오톡 미실행(`app_not_running`), 미로그인(`OnlineMainView` 미존재 → `not_logged_in`), 잠금모드 활성(`LockModeView` 가시 상태 → `lock_mode_active`), 모달 팝업 존재(`modal_dialog_present`), 동명 열린 창 복수 존재(`target_room_window_ambiguous`) 시 즉시 차단함.
   - 입력창(`RICHEDIT50W`)에 1글자라도 기존 초안이 있으면 전송과 창 닫기를 모두 차단(`existing_draft_preserved`)함.
   - `require_self=True` 지정 시 Windows에서 본인 채팅방('나와의 채팅' / 내 프로필 방)임이 확실히 검증된 경우만 허용하고 불확실하면 즉시 차단함.

---

## 7. 검증 계획 (구현 단계 진입 시)

1. **크로스플랫폼 단위 테스트 (`tests/`)**:
   - macOS·Linux·Windows 어디서나 실행 가능한 Mock Win32 계층 기반으로 클립보드 대화 파서(일반 대화, 멀티라인 본문, 사진/파일/이모티콘 행, 날짜 변경선, 동명 참여자), 초안 보존 차단, `operation_id` 멱등성·무재시도, 클립보드·포커스 복원 불변 조건을 `unittest`로 검증.
2. **Windows 실기기 통합 검증 (`Windows` Space)**:
   - 본인 채팅방(`require_self=True`) 및 테스트 방을 대상으로 13개 도구 전체 호출, 한글/이모지/멀티라인 전송 및 `send_result` 확인, 작성 중 초안 차단, 잠금모드 감지, 클립보드 이미지/텍스트 복원 여부를 실측 검증.

## 8. 구현 및 검증 기록

- 실제 `origin/main`인 `4fb2bc7`까지 fast-forward했다. 명세의 13개 도구 기준 커밋 `da3ebba`와 이후 두 커밋을 포함한다.
- Windows 백엔드와 MCP stdio는 표준 라이브러리로 실행한다. 새 Windows 가상환경에서 `pip install -e .` 후 `kax build`가 통과했다.
- macOS 단위 테스트 91개가 통과했고 Swift 브리지 3종을 빌드했다. Windows는 88개가 통과했고 macOS 전용 Swift 테스트 3개를 건너뛰었다. Windows에 설치한 MCP 서버에서 13개 도구와 상태 조회를 확인했다.
- Windows 11 `10.0.26200`, Python 3.12.10 x64, 카카오톡 `26.10.0.5364`가 실행 중인 `Windows` Space에서 13개 도구를 호출했다. 방 목록과 검색 호출 전후 열린 창 목록이 같았다. 이후 본인 방에서만 열기·읽기·전송·닫기를 수행했다.
- 한글·이모지·여러 줄 본문이 한 번 전송됐고 같은 요청 ID로 `send_result`가 `observed_in_ax`를 반환했다. 동일 ID 재호출의 전송 횟수는 0이었다. 사진 전송 확인 모달에서는 본문 전송이 차단됐고, 초안이 있을 때 전송과 창 닫기가 차단됐다. 본문 검색 바를 닫은 뒤에도 초안이 그대로였다.
- 사진 픽셀은 192×132 PNG로 확인했다. 창을 줄여 사진을 일부 숨긴 뒤 스크롤을 거쳐 사진만 캡처했고, 작업 전 스크롤 위치와 창 크기를 확인했다. 최신 스냅샷 확인과 OCR 옵션도 검증했다.
- Windows의 미로그인·잠금모드 차단은 모의 테스트에서 검증했다. 실계정은 로그인 상태에서 사용했다. Windows 10은 실기기 검증 범위에 포함되지 않았다.
- 후속 클립보드 검증에서 Unicode 텍스트·DIB 이미지·RTF 바이트를 그대로 복원했다. 초기 사진 준비 프로브에서는 클립보드의 시퀀스가 바뀐 뒤 원래 내용을 복구하지 못했다. 해당 Windows 세션의 클립보드 기록은 비활성화 상태였다. 테스트 창, 검색어와 포커스는 복원했다.
