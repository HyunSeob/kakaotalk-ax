#!/usr/bin/env python3
"""Native macOS and Windows KakaoTalk tools over MCP stdio."""

import json
import sys

if sys.platform == "win32":
    from kakaotalk_mcp_stdio import StdioMCP as FastMCP, ToolAnnotations, TextContent, ImageContent, Image
else:
    from mcp.server.fastmcp import FastMCP
    from mcp.types import ToolAnnotations, TextContent, ImageContent
    from mcp.server.fastmcp.utilities.types import Image

mcp = FastMCP("kakaotalk-ax", instructions=(
    "카카오톡은 macOS AX와 Windows Win32 네이티브 헬퍼에서 같은 kakaotalk_ax_* 도구를 사용합니다. "
    "방 열기·닫기·읽기·일괄 조회·검색·이전 대화 요청·전송을 지원합니다. "
    "여러 방은 kakaotalk_ax_read_many를 사용하세요. macOS는 병렬, Windows는 순차 조회합니다. "
    "읽기는 정확한 제목의 열린 방에서 앱이 내보낸 행을 대상으로 합니다. 다음 페이지에 snapshotId를 전달하세요. "
    "Windows 방 목록은 읽음 처리 없이 캡처한 OCR 결과입니다. 정확한 원문 제목은 열린 창에서 확인하세요. "
    "사용자가 명시적으로 요청한 방과 본문에만 전송하고 요청별 operation_id를 한 번 정해 재사용하세요. "
    "Windows는 정확한 제목의 창이 이미 열려 있어야 전송하며, 두 플랫폼 모두 기존 초안을 보존합니다. "
    "outcome_unknown이나 타임아웃이면 원래 ID로 kakaotalk_ax_send_result만 조회하고 재전송하지 마세요. "
    "observed_in_ax는 새 텍스트와 빈 입력창의 관찰이며 작성자·서버 ACK·읽음 확인이 아닙니다. "
    "사진은 kakaotalk_ax_read_attachment로 이미지 파일을 받아 직접 확인하세요. OCR은 선택 사항이며 원문과 구분하세요."
))
READ = ToolAnnotations(readOnlyHint=True, destructiveHint=False, idempotentHint=True, openWorldHint=False)
SEND = ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True)
UI = ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=True, openWorldHint=True)
NAVIGATE = ToolAnnotations(readOnlyHint=False, destructiveHint=False, idempotentHint=False, openWorldHint=True)


def output(value):
    return json.dumps(value, ensure_ascii=False, indent=2)


@mcp.tool(annotations=READ)
def kakaotalk_ax_windows() -> str:
    """현재 열린 카카오톡 창의 원문 제목과 읽기 가능 여부를 확인합니다.

    방을 열거나 창을 올리지 않으며, 메시지 본문·입력 중인 초안을 읽지 않습니다.
    readable=true인 창의 정확한 windowTitle을 kakaotalk_ax_read에 전달하세요.
    닫힌 방 목록이나 계정의 모든 방을 반환하는 도구가 아닙니다.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().windows())


@mcp.tool(annotations=READ)
def kakaotalk_ax_read(window_title: str, limit: int = 50, offset: int = 0, snapshot_id: str = "") -> str:
    """열린 방에서 macOS AX 또는 Windows 클립보드 원문을 읽습니다. limit 1..500.

    최신 행부터 페이지를 선택하고 페이지 안에서는 화면의 행 순서를 유지합니다.
    다음 페이지는 nextOffset과 snapshotId를 offset/snapshot_id로 전달하세요.
    새 메시지 등으로 행이 바뀌면 snapshot_changed를 반환합니다. offset=0부터 다시 읽으세요.
    이미지는 attachment로 보존합니다. Windows는 복사본의 작성자·날짜·시각을 반환하며 macOS의 미노출 값은 null입니다.
    hasMore는 수집한 최대 1000행 안의 페이지 유무이며, 전체 대화의 완전성을 뜻하지 않습니다.
    Windows는 잠시 대화창 포커스와 클립보드를 사용하고 복원합니다. 사용자 입력과 충돌하면 중단합니다.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().read_messages(window_title, limit, offset, snapshot_id))


@mcp.tool(annotations=NAVIGATE)
def kakaotalk_ax_read_attachment(window_title: str, row_index: int, snapshot_id: str,
                                 output_path: str | None = None, ocr: bool = False) -> list[TextContent | ImageContent]:
    """열린 창의 이미지 첨부 영역을 PNG로 반환합니다.

    read의 최신 snapshotId/rowIndex가 필요합니다. macOS 14.2+ 및 화면 기록 권한이 필요합니다.
    Windows는 PrintWindow로 캡처합니다. 잠시 스크롤한 뒤 가능한 경우 위치를 복원합니다.
    imagePath의 이미지를 직접 보세요. output_path 미지정 시 임시 PNG입니다.
    ocr=True일 때 로컬 Vision 또는 Windows 내장 OCR을 별도 필드로 제공합니다.
    """
    from kakaotalk_automation import NativeAX
    result = NativeAX().read_attachment(window_title, row_index, snapshot_id, output_path, ocr)
    content: list[TextContent | ImageContent] = [TextContent(type="text", text=output(result))]
    if result.get("status") == "ok" and result.get("imagePath"):
        content.append(Image(path=result["imagePath"]).to_image_content())
    return content


@mcp.tool(annotations=UI)
def kakaotalk_ax_rooms(query: str = "") -> str:
    """채팅 목록에서 보이는 방 이름·미리보기를 반환합니다. Windows는 OCR과 안 읽음/본인('나') 배지를 사용합니다.

    query는 현재 노출된 이름에 대한 부분 문자열 필터입니다. 모든 방의 완전한 목록이 아닙니다.
    목록과 검색은 방을 열지 않습니다. Windows OCR 결과는 textExact=false입니다.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().rooms(query))


@mcp.tool(annotations=UI)
def kakaotalk_ax_search_rooms(query: str) -> str:
    """공식 앱의 채팅방 검색창에 query를 설정하고 표시된 결과를 반환합니다. 빈 문자열은 검색 해제입니다."""
    from kakaotalk_automation import NativeAX
    return output(NativeAX().search_rooms(query))


@mcp.tool(annotations=UI)
def kakaotalk_ax_open(window_title: str, require_self: bool = False) -> str:
    """지정한 채팅방을 열고 실제 창의 원문 제목을 반환합니다. 동명 방은 중단합니다.

    이미 열린 창은 다시 열지 않습니다. 목록에 없으면 앱의 방 검색을 사용합니다.
    require_self=True는 앱에서 본인 채팅방이 확인된 경우만 허용합니다.
    Windows에서 본인 프로필명을 모를 때는 window_title='나와의 채팅'(또는 '나')과 require_self=True로 열어 정확한 windowTitle을 확인한 뒤 그 제목으로 읽기·전송하세요.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().open_room(window_title, require_self))


@mcp.tool(annotations=UI)
def kakaotalk_ax_close(window_title: str) -> str:
    """정확한 대상 대화창만 닫습니다. 방 나가기·앱 종료가 아닙니다. 기존 초안이 있으면 보존하고 중단합니다."""
    from kakaotalk_automation import NativeAX
    return output(NativeAX().close_room(window_title))


@mcp.tool(annotations=UI)
def kakaotalk_ax_read_many(window_titles: list[str], limit: int = 50, open_if_needed: bool = True,
                          close_after: bool = False, max_workers: int = 3) -> str:
    """1~20개 방을 읽습니다. macOS는 최대 4개 프로세스로 병렬, Windows는 순차 조회합니다.

    방별 결과와 실패를 각각 반환합니다. 같은 시점의 원자적 스냅샷은 아닙니다.
    close_after=True일 때 이 호출이 새로 연 창만 닫습니다. 기존 창과 초안은 보존합니다.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().read_many(window_titles, limit, open_if_needed, close_after, max_workers))


@mcp.tool(annotations=NAVIGATE)
def kakaotalk_ax_load_more(window_title: str, pages: int = 1) -> str:
    """열린 방의 대화 스크롤 영역에서 이전 대화 로딩을 최대 10번 요청하고 다시 읽습니다.

    steps, contentChanged, historyComplete를 확인하세요. 행 수가 그대로여도 과거 기록이 더 있을 수 있습니다.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().load_more(window_title, pages))


@mcp.tool(annotations=UI)
def kakaotalk_ax_search(window_title: str, query: str, limit: int = 100, load_pages: int = 0,
                        case_sensitive: bool = False, use_app_search: bool = True, max_steps: int = 10, context_rows: int = 2) -> str:
    """앱 검색의 이전 결과를 최대 max_steps(1..50)회 방문해 본문과 주변 context_rows행을 반환합니다.

    load_pages=1..10이면 검색 전에 이전 대화 로딩을 요청합니다.
    use_app_search=False면 앱 검색창을 건드리지 않고 노출된 본문에서만 문자 그대로 검색합니다.
    searchVerified/stopReason으로 결과 확인 여부와 중단 이유를 확인하세요. 겹치는 스냅샷의 동일 메시지가 반복될 수 있습니다. 검색 결과는 searchScope/coverage 범위에 한정됩니다. 전체 과거의 모든 일치 항목을 보장하지 않습니다.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().search_messages(window_title, query, limit, load_pages, case_sensitive, use_app_search, max_steps, context_rows))


@mcp.tool(annotations=UI)
def kakaotalk_ax_close_search(window_title: str) -> str:
    """열린 대화창의 검색 UI를 닫습니다. 메시지 입력창의 초안은 변경하지 않습니다."""
    from kakaotalk_automation import NativeAX
    return output(NativeAX().close_search(window_title))


@mcp.tool(annotations=SEND)
def kakaotalk_ax_send(window_title: str, text: str, operation_id: str, wait_seconds: float = 3,
                      require_self: bool = False) -> str:
    """기준 본문을 읽고 한 번 전송한 뒤 새 본문과 빈 입력창을 확인합니다.

    Windows는 이미 열린 창의 정확한 원문 제목을 요구합니다. 작성 중인 초안이 있으면 중단합니다.
    operation_id를 요청별 한 번 생성하고 재사용하세요. uncertain/타임아웃은 재발송하지 마세요.
    observed_in_ax는 새 텍스트 행과 빈 입력창을 관찰한 결과이며, 작성자·서버 ACK·상대방 읽음 확인이 아닙니다.
    같은 ID는 다시 보내지 않습니다. 결과 확인은 kakaotalk_ax_send_result를 사용하세요.
    """
    from kakaotalk_automation import NativeAX
    return output(NativeAX().send_message(window_title, text, operation_id, wait_seconds, require_self))


@mcp.tool(annotations=READ)
def kakaotalk_ax_send_result(operation_id: str, wait_seconds: float = 0) -> str:
    """전송 기록을 조회하고 필요시 읽기로 재확인합니다. 창 열기와 재전송은 수행하지 않습니다."""
    from kakaotalk_automation import NativeAX
    return output(NativeAX().send_result(operation_id, wait_seconds))


def main():
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
