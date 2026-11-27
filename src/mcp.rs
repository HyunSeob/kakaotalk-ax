//! Local MCP server over JSON-RPC 2.0 stdio exposing all 15 `kakaotalk_ax_*` tools.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use base64::Engine as _;
use serde_json::{json, Map, Value};

use crate::ax_core::KakaoBackend;
use crate::error::KaxError;
use crate::macos_ax_logic::{DEFAULT_MEMBER_LIMIT, DEFAULT_ROOM_SCROLL_LIMIT};

pub const MCP_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const SERVER_NAME: &str = "kakaotalk-ax";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const SERVER_INSTRUCTIONS: &str = concat!(
    "카카오톡은 macOS AX와 Windows Win32 네이티브 헬퍼에서 같은 kakaotalk_ax_* 도구를 사용합니다. ",
    "방 열기·닫기·읽기·일괄 조회·검색·이전 대화 요청·전송을 지원합니다. ",
    "여러 방은 kakaotalk_ax_read_many를 사용하세요. macOS는 병렬, Windows는 순차 조회합니다. ",
    "읽기는 정확한 제목의 열린 방에서 앱이 내보낸 행을 대상으로 합니다. 다음 페이지에 snapshotId를 전달하세요. ",
    "Windows 방 목록은 읽음 처리 없이 캡처한 OCR 결과입니다. 정확한 원문 제목은 열린 창에서 확인하세요. ",
    "사용자가 명시적으로 요청한 방과 본문에만 전송하고 요청별 operation_id를 한 번 정해 재사용하세요. ",
    "Windows는 정확한 제목의 창이 이미 열려 있어야 전송하며, 두 플랫폼 모두 기존 초안을 보존합니다. ",
    "outcome_unknown이나 타임아웃이면 원래 ID로 kakaotalk_ax_send_result만 조회하고 재전송하지 마세요. ",
    "observed_in_ax는 새 텍스트와 빈 입력창의 관찰이며 작성자·서버 ACK·읽음 확인이 아닙니다. ",
    "사진은 kakaotalk_ax_read_attachment로 이미지 파일을 받아 직접 확인하세요. OCR은 선택 사항이며 원문과 구분하세요."
);

fn annotations(read_only: bool, destructive: bool, idempotent: bool, open_world: bool) -> Value {
    json!({
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": open_world,
    })
}

fn string_output_schema(tool_name: &str) -> Value {
    json!({
        "properties": {
            "result": {
                "title": "Result",
                "type": "string"
            }
        },
        "required": ["result"],
        "title": format!("{tool_name}Output"),
        "type": "object"
    })
}

fn attachment_output_schema() -> Value {
    json!({
        "$defs": {
            "Annotations": {
                "additionalProperties": true,
                "properties": {
                    "audience": {
                        "anyOf": [
                            {
                                "items": {
                                    "enum": ["user", "assistant"],
                                    "type": "string"
                                },
                                "type": "array"
                            },
                            {"type": "null"}
                        ],
                        "default": null,
                        "title": "Audience"
                    },
                    "priority": {
                        "anyOf": [
                            {"maximum": 1.0, "minimum": 0.0, "type": "number"},
                            {"type": "null"}
                        ],
                        "default": null,
                        "title": "Priority"
                    }
                },
                "title": "Annotations",
                "type": "object"
            },
            "ImageContent": {
                "additionalProperties": true,
                "description": "Image content for a message.",
                "properties": {
                    "type": {"const": "image", "title": "Type", "type": "string"},
                    "data": {"title": "Data", "type": "string"},
                    "mimeType": {"title": "Mimetype", "type": "string"},
                    "annotations": {
                        "anyOf": [
                            {"$ref": "#/$defs/Annotations"},
                            {"type": "null"}
                        ],
                        "default": null
                    },
                    "_meta": {
                        "anyOf": [
                            {"additionalProperties": true, "type": "object"},
                            {"type": "null"}
                        ],
                        "default": null,
                        "title": "Meta"
                    }
                },
                "required": ["type", "data", "mimeType"],
                "title": "ImageContent",
                "type": "object"
            },
            "TextContent": {
                "additionalProperties": true,
                "description": "Text content for a message.",
                "properties": {
                    "type": {"const": "text", "title": "Type", "type": "string"},
                    "text": {"title": "Text", "type": "string"},
                    "annotations": {
                        "anyOf": [
                            {"$ref": "#/$defs/Annotations"},
                            {"type": "null"}
                        ],
                        "default": null
                    },
                    "_meta": {
                        "anyOf": [
                            {"additionalProperties": true, "type": "object"},
                            {"type": "null"}
                        ],
                        "default": null,
                        "title": "Meta"
                    }
                },
                "required": ["type", "text"],
                "title": "TextContent",
                "type": "object"
            }
        },
        "properties": {
            "result": {
                "items": {
                    "anyOf": [
                        {"$ref": "#/$defs/TextContent"},
                        {"$ref": "#/$defs/ImageContent"}
                    ]
                },
                "title": "Result",
                "type": "array"
            }
        },
        "required": ["result"],
        "title": "kakaotalk_ax_read_attachmentOutput",
        "type": "object"
    })
}

/// Wire-format compatibility profile matching baseline `e0127d0`:
/// - macOS used `mcp.server.fastmcp.FastMCP` (`outputSchema`, `title`, `structuredContent`).
/// - Windows used stdlib `kakaotalk_mcp_stdio.StdioMCP` (`additionalProperties: false`, no `outputSchema`/`title`/`structuredContent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProtocolStyle {
    MacFastMcp,
    WindowsStdio,
}

impl McpProtocolStyle {
    pub const fn platform_default() -> Self {
        #[cfg(windows)]
        {
            Self::WindowsStdio
        }
        #[cfg(not(windows))]
        {
            Self::MacFastMcp
        }
    }
}

fn to_windows_stdio_tool(mut tool: Value) -> Value {
    if let Some(obj) = tool.as_object_mut() {
        obj.remove("outputSchema");
        if let Some(desc) = obj.get("description").and_then(|v| v.as_str()) {
            obj.insert("description".to_string(), json!(desc.trim_end_matches('\n')));
        }
        if let Some(schema) = obj.get_mut("inputSchema").and_then(|v| v.as_object_mut()) {
            schema.remove("title");
            if !schema.contains_key("required") {
                schema.insert("required".to_string(), json!([]));
            }
            schema.insert("additionalProperties".to_string(), json!(false));
            if let Some(props) = schema.get_mut("properties").and_then(|v| v.as_object_mut()) {
                for prop in props.values_mut() {
                    if let Some(pobj) = prop.as_object_mut() {
                        pobj.remove("title");
                    }
                }
            }
        }
    }
    tool
}

/// Exact 15 MCP tool definitions for the current platform's `e0127d0` baseline.
pub fn tool_definitions() -> Vec<Value> {
    tool_definitions_for(McpProtocolStyle::platform_default())
}

/// Exact 15 MCP tool definitions for an explicit platform wire profile.
pub fn tool_definitions_for(style: McpProtocolStyle) -> Vec<Value> {
    let tools = fastmcp_tool_definitions();
    match style {
        McpProtocolStyle::MacFastMcp => tools,
        McpProtocolStyle::WindowsStdio => tools.into_iter().map(to_windows_stdio_tool).collect(),
    }
}

fn fastmcp_tool_definitions() -> Vec<Value> {
    let read = annotations(true, false, true, false);
    let send = annotations(false, false, false, true);
    let ui = annotations(false, false, true, true);
    let navigate = annotations(false, false, false, true);

    vec![
        json!({
            "name": "kakaotalk_ax_windows",
            "description": "현재 열린 카카오톡 창의 원문 제목과 읽기 가능 여부를 확인합니다.\n\n방을 열거나 창을 올리지 않으며, 메시지 본문·입력 중인 초안을 읽지 않습니다.\nreadable=true인 창의 정확한 windowTitle을 kakaotalk_ax_read에 전달하세요.\n닫힌 방 목록이나 계정의 모든 방을 반환하는 도구가 아닙니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_windowsArguments",
                "properties": {}
            },
            "outputSchema": string_output_schema("kakaotalk_ax_windows"),
            "annotations": read
        }),
        json!({
            "name": "kakaotalk_ax_read",
            "description": "열린 방에서 macOS AX 또는 Windows 클립보드 원문을 읽습니다. limit 1..500.\n\n최신 행부터 페이지를 선택하고 페이지 안에서는 화면의 행 순서를 유지합니다.\n다음 페이지는 nextOffset과 snapshotId를 offset/snapshot_id로 전달하세요.\n새 메시지 등으로 행이 바뀌면 snapshot_changed를 반환합니다. offset=0부터 다시 읽으세요.\n이미지는 attachment로 보존합니다. Windows는 복사본의 작성자·날짜·시각을 반환합니다.\nmacOS 항목은 sender(그 행에 보이는 이름), isSelf, groupSender·senderSource(이어받은 보낸 사람과 근거: row·inherited·self_layout·not_own_layout), timeText·groupTimeText, date·dateText(날짜 구분선 기준), unreadCount, edited, truncated(500자에서 잘림)를 줍니다. kind는 text·attachment·file(file.name·size·expiresAt)·system(systemType·target)·date_separator·unclassified입니다. textMessageCount 등 개수는 이전 버전 분류로 셉니다(system은 text, date_separator·file은 unclassified). 알 수 없는 값은 null입니다.\nhasMore는 수집한 최대 1000행 안의 페이지 유무이며, 전체 대화의 완전성을 뜻하지 않습니다.\nWindows는 잠시 대화창 포커스와 클립보드를 사용하고 복원합니다. 사용자 입력과 충돌하면 중단합니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_readArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "limit": {"title": "Limit", "type": "integer", "default": 50},
                    "offset": {"title": "Offset", "type": "integer", "default": 0},
                    "snapshot_id": {"title": "Snapshot Id", "type": "string", "default": ""}
                },
                "required": ["window_title"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_read"),
            "annotations": read
        }),
        json!({
            "name": "kakaotalk_ax_read_attachment",
            "description": "열린 창의 이미지 첨부 영역을 PNG로 반환합니다.\n\nread의 최신 snapshotId/rowIndex가 필요합니다. macOS 14.2+ 및 화면 기록 권한이 필요합니다.\nWindows는 PrintWindow로 캡처합니다. 잠시 스크롤한 뒤 가능한 경우 위치를 복원합니다.\nimagePath의 이미지를 직접 보세요. output_path 미지정 시 임시 PNG입니다.\nocr=True일 때 로컬 Vision 또는 Windows 내장 OCR을 별도 필드로 제공합니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_read_attachmentArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "row_index": {"title": "Row Index", "type": "integer"},
                    "snapshot_id": {"title": "Snapshot Id", "type": "string"},
                    "output_path": {"title": "Output Path", "anyOf": [{"type": "string"}, {"type": "null"}], "default": null},
                    "ocr": {"title": "Ocr", "type": "boolean", "default": false}
                },
                "required": ["window_title", "row_index", "snapshot_id"]
            },
            "outputSchema": attachment_output_schema(),
            "annotations": navigate
        }),
        json!({
            "name": "kakaotalk_ax_rooms",
            "description": "채팅 목록에서 보이는 방 이름·미리보기를 반환합니다. Windows는 OCR과 안 읽음/본인('나') 배지를 사용합니다.\n\nquery는 현재 노출된 이름에 대한 부분 문자열 필터입니다. 기본값은 모든 방의 완전한 목록이 아닙니다.\nmacOS에서 scroll=true면 채팅 목록을 맨 위부터 스크롤하며 최대 limit개(기본 300, 최대 1000)를 모은 뒤 원래 스크롤 위치로 되돌립니다. 그동안 목록이 화면에서 움직입니다. Windows는 scroll을 지원하지 않습니다.\nmacOS에서 folder(예: '안읽음')를 주면 그 채팅방 폴더로 바꿔 읽고 원래 폴더로 되돌립니다. 폴더 이름은 kakaotalk_ax_folders로 확인하세요. 각 항목에는 unread(안 읽은 수), memberCount, lastMessageTime, muted, pinned가 있습니다.\n목록과 검색은 방을 열지 않습니다. Windows OCR 결과는 textExact=false입니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_roomsArguments",
                "properties": {
                    "query": {"title": "Query", "type": "string", "default": ""},
                    "scroll": {"title": "Scroll", "type": "boolean", "default": false},
                    "limit": {"title": "Limit", "type": "integer", "default": 300},
                    "folder": {"title": "Folder", "anyOf": [{"type": "string"}, {"type": "null"}], "default": null}
                }
            },
            "outputSchema": string_output_schema("kakaotalk_ax_rooms"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_folders",
            "description": "채팅 목록 위의 채팅방 폴더(전체·안읽음·사용자 폴더)와 폴더별 안 읽은 메시지 수, 현재 선택된 폴더를 반환합니다(macOS). 방을 열거나 폴더를 바꾸지 않습니다.\n안 읽은 방만 보려면 kakaotalk_ax_rooms(folder='안읽음')을 사용하세요. Windows는 folder_unsupported입니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_foldersArguments",
                "properties": {}
            },
            "outputSchema": string_output_schema("kakaotalk_ax_folders"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_search_rooms",
            "description": "공식 앱의 채팅방 검색창에 query를 설정하고 표시된 결과를 반환합니다. 빈 문자열은 검색 해제입니다. macOS는 검색 바도 닫고(이미 닫혀 있으면 그대로) 결과의 searchOpen으로 검색 바가 열려 있는지 알려 줍니다.",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_search_roomsArguments",
                "properties": {
                    "query": {"title": "Query", "type": "string"}
                },
                "required": ["query"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_search_rooms"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_open",
            "description": "지정한 채팅방을 열고 실제 창의 원문 제목을 반환합니다. 동명 방은 중단합니다.\n\n이미 열린 창은 다시 열지 않습니다. 목록에 없으면 앱의 방 검색을 사용합니다.\nrequire_self=True는 앱에서 본인 채팅방이 확인된 경우만 허용합니다.\nWindows에서 본인 프로필명을 모를 때는 window_title='나와의 채팅'(또는 '나')과 require_self=True로 열어 정확한 windowTitle을 확인한 뒤 그 제목으로 읽기·전송하세요.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_openArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "require_self": {"title": "Require Self", "type": "boolean", "default": false}
                },
                "required": ["window_title"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_open"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_close",
            "description": "정확한 대상 대화창만 닫습니다. 방 나가기·앱 종료가 아닙니다. 기존 초안이 있으면 보존하고 중단합니다.",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_closeArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"}
                },
                "required": ["window_title"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_close"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_members",
            "description": "열린 단체방(일반·팀·오픈채팅)의 대화상대 목록을 읽습니다. 방을 열지 않으니 먼저 kakaotalk_ax_open으로 여세요.\n\nmacOS에서 창의 '대화상대 N명' 버튼으로 목록을 열어 최대 limit명(기본 1000, 최대 3000)을 읽고 다시 닫습니다. 목록 검색창은 건드리지 않습니다.\n각 항목은 name, rowIndex, isSelf, role('owner'=방장·팀장, 'staff'=부방장, null), badges입니다. memberCount는 버튼의 인원 수입니다.\n1:1방·나와의 채팅은 member_list_unavailable, Windows는 member_list_unsupported입니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_membersArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "limit": {"title": "Limit", "type": "integer", "default": 1000}
                },
                "required": ["window_title"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_members"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_read_many",
            "description": "1~20개 방을 읽습니다. macOS는 최대 4개 프로세스로 병렬, Windows는 순차 조회합니다.\n\n방별 결과와 실패를 각각 반환합니다. 같은 시점의 원자적 스냅샷은 아닙니다.\nclose_after=True일 때 이 호출이 새로 연 창만 닫습니다. 기존 창과 초안은 보존합니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_read_manyArguments",
                "properties": {
                    "window_titles": {"title": "Window Titles", "type": "array", "items": {"type": "string"}},
                    "limit": {"title": "Limit", "type": "integer", "default": 50},
                    "open_if_needed": {"title": "Open If Needed", "type": "boolean", "default": true},
                    "close_after": {"title": "Close After", "type": "boolean", "default": false},
                    "max_workers": {"title": "Max Workers", "type": "integer", "default": 3}
                },
                "required": ["window_titles"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_read_many"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_load_more",
            "description": "열린 방의 대화 스크롤 영역에서 이전 대화 로딩을 최대 10번 요청하고 다시 읽습니다.\n\nmacOS 기본 응답은 요약입니다(실패 응답과 Windows 결과는 그대로): 늘어난 행 수, 단계별 행 수, oldestDate·newestDate, 새 snapshotId, 1000행 제한으로 잘렸는지(rowsTruncated, truncatedBefore, warning). 본문은 snapshotId로 kakaotalk_ax_read(offset, limit)를 나눠 읽으세요. include_items=true면 예전처럼 steps와 after(본문)를 함께 돌려줍니다.\ncontentChanged, historyComplete를 확인하세요. 행 수가 그대로여도 과거 기록이 더 있을 수 있습니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_load_moreArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "pages": {"title": "Pages", "type": "integer", "default": 1},
                    "include_items": {"title": "Include Items", "type": "boolean", "default": false}
                },
                "required": ["window_title"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_load_more"),
            "annotations": navigate
        }),
        json!({
            "name": "kakaotalk_ax_search",
            "description": "앱 검색의 이전 결과를 최대 max_steps(1..50)회 방문해 본문과 주변 context_rows행을 반환합니다.\n\nload_pages=1..10이면 검색 전에 이전 대화 로딩을 요청합니다.\nuse_app_search=False면 앱 검색창을 건드리지 않고 노출된 본문에서만 문자 그대로 검색합니다.\nsearchVerified/stopReason으로 결과 확인 여부와 중단 이유를 확인하세요. 겹치는 스냅샷의 동일 메시지가 반복될 수 있습니다. 검색 결과는 searchScope/coverage 범위에 한정됩니다. 전체 과거의 모든 일치 항목을 보장하지 않습니다.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_searchArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "query": {"title": "Query", "type": "string"},
                    "limit": {"title": "Limit", "type": "integer", "default": 100},
                    "load_pages": {"title": "Load Pages", "type": "integer", "default": 0},
                    "case_sensitive": {"title": "Case Sensitive", "type": "boolean", "default": false},
                    "use_app_search": {"title": "Use App Search", "type": "boolean", "default": true},
                    "max_steps": {"title": "Max Steps", "type": "integer", "default": 10},
                    "context_rows": {"title": "Context Rows", "type": "integer", "default": 2}
                },
                "required": ["window_title", "query"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_search"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_close_search",
            "description": "열린 대화창의 검색 UI를 닫습니다. 메시지 입력창의 초안은 변경하지 않습니다.",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_close_searchArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"}
                },
                "required": ["window_title"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_close_search"),
            "annotations": ui
        }),
        json!({
            "name": "kakaotalk_ax_send",
            "description": "기준 본문을 읽고 한 번 전송한 뒤 새 본문과 빈 입력창을 확인합니다.\n\nWindows는 이미 열린 창의 정확한 원문 제목을 요구합니다. 작성 중인 초안이 있으면 중단합니다.\noperation_id를 요청별 한 번 생성하고 재사용하세요. uncertain/타임아웃은 재발송하지 마세요.\nobserved_in_ax는 새 텍스트 행과 빈 입력창을 관찰한 결과이며, 작성자·서버 ACK·상대방 읽음 확인이 아닙니다.\n같은 ID는 다시 보내지 않습니다. 결과 확인은 kakaotalk_ax_send_result를 사용하세요.\n",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_sendArguments",
                "properties": {
                    "window_title": {"title": "Window Title", "type": "string"},
                    "text": {"title": "Text", "type": "string"},
                    "operation_id": {"title": "Operation Id", "type": "string"},
                    "wait_seconds": {"title": "Wait Seconds", "type": "number", "default": 3},
                    "require_self": {"title": "Require Self", "type": "boolean", "default": false}
                },
                "required": ["window_title", "text", "operation_id"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_send"),
            "annotations": send
        }),
        json!({
            "name": "kakaotalk_ax_send_result",
            "description": "전송 기록을 조회하고 필요시 읽기로 재확인합니다. 창 열기와 재전송은 수행하지 않습니다.",
            "inputSchema": {
                "type": "object",
                "title": "kakaotalk_ax_send_resultArguments",
                "properties": {
                    "operation_id": {"title": "Operation Id", "type": "string"},
                    "wait_seconds": {"title": "Wait Seconds", "type": "number", "default": 0}
                },
                "required": ["operation_id"]
            },
            "outputSchema": string_output_schema("kakaotalk_ax_send_result"),
            "annotations": read
        }),
    ]
}

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub struct McpServer<'a> {
    backend: &'a dyn KakaoBackend,
    style: McpProtocolStyle,
}

impl<'a> McpServer<'a> {
    pub fn new(backend: &'a dyn KakaoBackend) -> Self {
        Self::with_style(backend, McpProtocolStyle::platform_default())
    }

    pub fn with_style(backend: &'a dyn KakaoBackend, style: McpProtocolStyle) -> Self {
        Self { backend, style }
    }

    pub fn serve<R: Read, W: Write>(&self, reader: R, mut writer: W) -> std::io::Result<()> {
        let mut buf_reader = BufReader::new(reader);
        let mut initialized = false;
        let mut ready = false;
        let mut line = Vec::new();

        loop {
            line.clear();
            let read_n = Read::by_ref(&mut buf_reader)
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut line)?;
            if read_n == 0 {
                break;
            }
            if line.len() > MAX_LINE_BYTES {
                break;
            }

            let mut request_id = Value::Null;
            let payload = match self.handle_line(&line, &mut initialized, &mut ready, &mut request_id) {
                Ok(Some(result)) => json!({"result": result}),
                Ok(None) => continue,
                Err(rpc_err) => json!({
                    "error": {
                        "code": rpc_err.code,
                        "message": rpc_err.message,
                    }
                }),
            };

            let mut msg = Map::new();
            msg.insert("jsonrpc".to_string(), json!("2.0"));
            msg.insert("id".to_string(), request_id);
            if let Value::Object(p) = payload {
                for (k, v) in p {
                    msg.insert(k, v);
                }
            }
            let encoded = serde_json::to_string(&Value::Object(msg))
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            writer.write_all(encoded.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        Ok(())
    }

    fn handle_line(
        &self,
        line: &[u8],
        initialized: &mut bool,
        ready: &mut bool,
        request_id: &mut Value,
    ) -> std::result::Result<Option<Value>, RpcError> {
        let text = std::str::from_utf8(line).map_err(|_| RpcError::new(-32700, "Parse error"))?;
        let request: Value =
            serde_json::from_str(text).map_err(|_| RpcError::new(-32700, "Parse error"))?;
        let Some(obj) = request.as_object() else {
            return Err(RpcError::new(-32600, "Invalid Request"));
        };
        if obj.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
            return Err(RpcError::new(-32600, "Invalid Request"));
        }
        let Some(method) = obj.get("method").and_then(|v| v.as_str()) else {
            return Err(RpcError::new(-32600, "Invalid Request"));
        };
        let params = match obj.get("params") {
            None => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(RpcError::new(-32600, "Invalid Request")),
        };

        if !obj.contains_key("id") {
            if method == "notifications/initialized" && *initialized {
                *ready = true;
            }
            return Ok(None);
        }

        let id = obj.get("id").cloned().unwrap_or(Value::Null);
        let valid_id = id.is_string() || (id.is_i64() || id.is_u64());
        if !valid_id {
            return Err(RpcError::new(-32600, "Invalid Request"));
        }
        *request_id = id;

        if method == "ping" {
            return Ok(Some(json!({})));
        }
        if method == "initialize" && !*initialized {
            let Some(version) = params.get("protocolVersion").and_then(|v| v.as_str()) else {
                return Err(RpcError::new(-32602, "protocolVersion is required"));
            };
            *initialized = true;
            let negotiated = if MCP_VERSIONS.contains(&version) {
                version
            } else {
                MCP_VERSIONS[0]
            };
            return Ok(Some(json!({
                "protocolVersion": negotiated,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
                "instructions": SERVER_INSTRUCTIONS,
            })));
        }
        if !*ready {
            return Err(RpcError::new(-32002, "Server not initialized"));
        }
        match method {
            "tools/list" => Ok(Some(json!({ "tools": tool_definitions_for(self.style) }))),
            "tools/call" => self.call_tool(&params).map(Some),
            _ => Err(RpcError::new(-32601, "Method not found")),
        }
    }

    fn call_tool(&self, params: &Map<String, Value>) -> std::result::Result<Value, RpcError> {
        let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
            return Err(RpcError::new(-32602, "Unknown tool or invalid arguments"));
        };
        let args = match params.get("arguments") {
            None => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(RpcError::new(-32602, "Unknown tool or invalid arguments")),
        };

        let known_tools = [
            "kakaotalk_ax_windows",
            "kakaotalk_ax_read",
            "kakaotalk_ax_read_attachment",
            "kakaotalk_ax_rooms",
            "kakaotalk_ax_folders",
            "kakaotalk_ax_search_rooms",
            "kakaotalk_ax_open",
            "kakaotalk_ax_close",
            "kakaotalk_ax_members",
            "kakaotalk_ax_read_many",
            "kakaotalk_ax_load_more",
            "kakaotalk_ax_search",
            "kakaotalk_ax_close_search",
            "kakaotalk_ax_send",
            "kakaotalk_ax_send_result",
        ];
        if !known_tools.contains(&name) {
            if self.style == McpProtocolStyle::MacFastMcp {
                return Ok(json!({
                    "isError": true,
                    "content": [{"type": "text", "text": format!("Unknown tool: {name}")}],
                }));
            }
            return Err(RpcError::new(-32602, "Unknown tool or invalid arguments"));
        }

        let mac_fastmcp = self.style == McpProtocolStyle::MacFastMcp;
        let res = dispatch_tool_inner(self.backend, name, &args, mac_fastmcp, mac_fastmcp);
        match res {
            Ok(val) => {
                let text = serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string());
                let mut content = vec![json!({"type": "text", "text": text})];
                if name == "kakaotalk_ax_read_attachment"
                    && val.get("status").and_then(|v| v.as_str()) == Some("ok")
                {
                    if let Some(path_str) = val.get("imagePath").and_then(|v| v.as_str()) {
                        if let Ok(bytes) = std::fs::read(Path::new(path_str)) {
                            let encoded =
                                base64::engine::general_purpose::STANDARD.encode(&bytes);
                            content.push(json!({
                                "type": "image",
                                "data": encoded,
                                "mimeType": "image/png",
                            }));
                        }
                    }
                }
                let mut out = json!({
                    "isError": false,
                    "content": content,
                });
                if self.style == McpProtocolStyle::MacFastMcp {
                    let structured = if name == "kakaotalk_ax_read_attachment" {
                        json!({"result": out["content"]})
                    } else {
                        json!({"result": text})
                    };
                    out["structuredContent"] = structured;
                }
                Ok(out)
            }
            Err(KaxError::Validation(msg)) => Ok(json!({
                "isError": true,
                "content": [{"type": "text", "text": msg}],
            })),
            Err(KaxError::Runtime { error_type, detail }) if error_type == "TypeError" => {
                Ok(json!({
                    "isError": true,
                    "content": [{"type": "text", "text": detail}],
                }))
            }
            Err(err) => Ok(json!({
                "isError": true,
                "content": [{"type": "text", "text": err.error_type()}],
            })),
        }
    }
}

/// Dispatch either an MCP tool name (`kakaotalk_ax_*`) or CLI method name (`windows`, `read_messages`, ...).
pub fn dispatch_method(
    backend: &dyn KakaoBackend,
    method: &str,
    args: &Map<String, Value>,
) -> crate::error::Result<Value> {
    let tool_name = match method {
        "windows" => "kakaotalk_ax_windows",
        "rooms" => "kakaotalk_ax_rooms",
        "folders" => "kakaotalk_ax_folders",
        "search_rooms" => "kakaotalk_ax_search_rooms",
        "open_room" => "kakaotalk_ax_open",
        "close_room" => "kakaotalk_ax_close",
        "members" => "kakaotalk_ax_members",
        "read_messages" => "kakaotalk_ax_read",
        "read_attachment" => "kakaotalk_ax_read_attachment",
        "read_many" => "kakaotalk_ax_read_many",
        "load_more" => "kakaotalk_ax_load_more",
        "search_messages" => "kakaotalk_ax_search",
        "close_search" => "kakaotalk_ax_close_search",
        "send_message" => "kakaotalk_ax_send",
        "send_result" => "kakaotalk_ax_send_result",
        other => other,
    };
    dispatch_tool(backend, tool_name, args)
}

fn check_allowed_keys(
    args: &Map<String, Value>,
    allowed: &[&str],
    allow_extra: bool,
) -> crate::error::Result<()> {
    if allow_extra {
        return Ok(());
    }
    for key in args.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(KaxError::runtime(
                "TypeError",
                format!("Unexpected argument: {key}"),
            ));
        }
    }
    Ok(())
}

fn req_str<'a>(args: &'a Map<String, Value>, key: &str) -> crate::error::Result<&'a str> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(KaxError::validation(
            "Argument type does not match the tool schema",
        )),
        None => Err(KaxError::runtime(
            "TypeError",
            format!("Missing required argument: {key}"),
        )),
    }
}

fn opt_str<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    default: &'a str,
) -> crate::error::Result<&'a str> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(KaxError::validation(
            "Argument type does not match the tool schema",
        )),
    }
}

fn opt_nullable_str<'a>(
    args: &'a Map<String, Value>,
    key: &str,
) -> crate::error::Result<Option<&'a str>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(KaxError::validation(
            "Argument type does not match the tool schema",
        )),
    }
}

fn parse_json_int(value: &Value, fastmcp_numbers: bool) -> Option<i64> {
    if let Value::String(text) = value {
        if !fastmcp_numbers {
            return None;
        }
        // FastMCP validates string arguments using Pydantic's integer parser.
        // Avoid f64 here: rounding could make a fractional string an integer.
        let text = text.trim();
        let (integer, fraction) = text.split_once('.').unwrap_or((text, "0"));
        if fraction.is_empty() || !fraction.bytes().all(|b| b == b'0') {
            return None;
        }
        let digits = integer.trim_start_matches(['+', '-']);
        if digits.split('_').any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit())) {
            return None;
        }
        return integer.replace('_', "").parse().ok();
    }

    let Value::Number(n) = value else {
        return None;
    };
    if let Some(i) = n.as_i64() {
        return Some(i);
    }
    if fastmcp_numbers {
        if let Some(f) = n.as_f64() {
            if f.is_finite()
                && f.fract() == 0.0
                && f >= (i64::MIN as f64)
                && f < (i64::MAX as f64)
            {
                return Some(f as i64);
            }
        }
    }
    None
}

fn req_int(
    args: &Map<String, Value>,
    key: &str,
    fastmcp_numbers: bool,
) -> crate::error::Result<i64> {
    match args.get(key) {
        Some(v) => parse_json_int(v, fastmcp_numbers).ok_or_else(|| {
            KaxError::validation("Argument type does not match the tool schema")
        }),
        None => Err(KaxError::runtime(
            "TypeError",
            format!("Missing required argument: {key}"),
        )),
    }
}

fn opt_int(
    args: &Map<String, Value>,
    key: &str,
    default: i64,
    fastmcp_numbers: bool,
) -> crate::error::Result<i64> {
    match args.get(key) {
        None => Ok(default),
        Some(v) => parse_json_int(v, fastmcp_numbers).ok_or_else(|| {
            KaxError::validation("Argument type does not match the tool schema")
        }),
    }
}

fn opt_f64(args: &Map<String, Value>, key: &str, default: f64, fastmcp_numbers: bool) -> crate::error::Result<f64> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| KaxError::validation("Argument type does not match the tool schema")),
        Some(Value::String(text)) if fastmcp_numbers => text.trim().parse::<f64>()
            .map_err(|_| KaxError::validation("Argument type does not match the tool schema")),
        Some(_) => Err(KaxError::validation(
            "Argument type does not match the tool schema",
        )),
    }
}

fn opt_bool(args: &Map<String, Value>, key: &str, default: bool) -> crate::error::Result<bool> {
    match args.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(KaxError::validation(
            "Argument type does not match the tool schema",
        )),
    }
}

fn req_str_list(args: &Map<String, Value>, key: &str) -> crate::error::Result<Vec<String>> {
    match args.get(key) {
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err(KaxError::validation(
                        "Argument type does not match the tool schema",
                    ));
                };
                out.push(s.to_string());
            }
            Ok(out)
        }
        Some(_) => Err(KaxError::validation(
            "Argument type does not match the tool schema",
        )),
        None => Err(KaxError::runtime(
            "TypeError",
            format!("Missing required argument: {key}"),
        )),
    }
}

pub fn dispatch_tool(
    backend: &dyn KakaoBackend,
    tool_name: &str,
    args: &Map<String, Value>,
) -> crate::error::Result<Value> {
    dispatch_tool_inner(backend, tool_name, args, false, false)
}

fn dispatch_tool_inner(
    backend: &dyn KakaoBackend,
    tool_name: &str,
    args: &Map<String, Value>,
    allow_extra: bool,
    fastmcp_numbers: bool,
) -> crate::error::Result<Value> {
    match tool_name {
        "kakaotalk_ax_windows" => {
            check_allowed_keys(args, &[], allow_extra)?;
            backend.windows()
        }
        "kakaotalk_ax_rooms" => {
            check_allowed_keys(args, &["query", "scroll", "limit", "folder"], allow_extra)?;
            backend.rooms(
                opt_str(args, "query", "")?,
                opt_bool(args, "scroll", false)?,
                opt_int(args, "limit", DEFAULT_ROOM_SCROLL_LIMIT, fastmcp_numbers)?,
                opt_nullable_str(args, "folder")?,
            )
        }
        "kakaotalk_ax_folders" => {
            check_allowed_keys(args, &[], allow_extra)?;
            backend.folders()
        }
        "kakaotalk_ax_members" => {
            check_allowed_keys(args, &["window_title", "limit"], allow_extra)?;
            backend.members(
                req_str(args, "window_title")?,
                opt_int(args, "limit", DEFAULT_MEMBER_LIMIT, fastmcp_numbers)?,
            )
        }
        "kakaotalk_ax_search_rooms" => {
            check_allowed_keys(args, &["query"], allow_extra)?;
            backend.search_rooms(req_str(args, "query")?)
        }
        "kakaotalk_ax_open" => {
            check_allowed_keys(args, &["window_title", "require_self"], allow_extra)?;
            backend.open_room(
                req_str(args, "window_title")?,
                opt_bool(args, "require_self", false)?,
            )
        }
        "kakaotalk_ax_close" => {
            check_allowed_keys(args, &["window_title"], allow_extra)?;
            backend.close_room(req_str(args, "window_title")?)
        }
        "kakaotalk_ax_read" => {
            check_allowed_keys(
                args,
                &["window_title", "limit", "offset", "snapshot_id"],
                allow_extra,
            )?;
            backend.read_messages(
                req_str(args, "window_title")?,
                opt_int(args, "limit", 50, fastmcp_numbers)?,
                opt_int(args, "offset", 0, fastmcp_numbers)?,
                opt_str(args, "snapshot_id", "")?,
            )
        }
        "kakaotalk_ax_read_attachment" => {
            check_allowed_keys(
                args,
                &[
                    "window_title",
                    "row_index",
                    "snapshot_id",
                    "output_path",
                    "ocr",
                ],
                allow_extra,
            )?;
            backend.read_attachment(
                req_str(args, "window_title")?,
                req_int(args, "row_index", fastmcp_numbers)?,
                req_str(args, "snapshot_id")?,
                opt_nullable_str(args, "output_path")?,
                opt_bool(args, "ocr", false)?,
            )
        }
        "kakaotalk_ax_read_many" => {
            check_allowed_keys(
                args,
                &[
                    "window_titles",
                    "limit",
                    "open_if_needed",
                    "close_after",
                    "max_workers",
                ],
                allow_extra,
            )?;
            let titles = req_str_list(args, "window_titles")?;
            backend.read_many(
                &titles,
                opt_int(args, "limit", 50, fastmcp_numbers)?,
                opt_bool(args, "open_if_needed", true)?,
                opt_bool(args, "close_after", false)?,
                opt_int(args, "max_workers", 3, fastmcp_numbers)?,
            )
        }
        "kakaotalk_ax_load_more" => {
            check_allowed_keys(args, &["window_title", "pages", "include_items"], allow_extra)?;
            let include_items = opt_bool(args, "include_items", false)?;
            let full = backend.load_more(
                req_str(args, "window_title")?,
                opt_int(args, "pages", 1, fastmcp_numbers)?,
            )?;
            Ok(if include_items { full } else { crate::ax_core::load_more_summary(&full) })
        }
        "kakaotalk_ax_search" => {
            check_allowed_keys(
                args,
                &[
                    "window_title",
                    "query",
                    "limit",
                    "load_pages",
                    "case_sensitive",
                    "use_app_search",
                    "max_steps",
                    "context_rows",
                ],
                allow_extra,
            )?;
            backend.search_messages(
                req_str(args, "window_title")?,
                req_str(args, "query")?,
                opt_int(args, "limit", 100, fastmcp_numbers)?,
                opt_int(args, "load_pages", 0, fastmcp_numbers)?,
                opt_bool(args, "case_sensitive", false)?,
                opt_bool(args, "use_app_search", true)?,
                opt_int(args, "max_steps", 10, fastmcp_numbers)?,
                opt_int(args, "context_rows", 2, fastmcp_numbers)?,
            )
        }
        "kakaotalk_ax_close_search" => {
            check_allowed_keys(args, &["window_title"], allow_extra)?;
            backend.close_search(req_str(args, "window_title")?)
        }
        "kakaotalk_ax_send" => {
            check_allowed_keys(
                args,
                &[
                    "window_title",
                    "text",
                    "operation_id",
                    "wait_seconds",
                    "require_self",
                ],
                allow_extra,
            )?;
            backend.send_message(
                req_str(args, "window_title")?,
                req_str(args, "text")?,
                req_str(args, "operation_id")?,
                opt_f64(args, "wait_seconds", 3.0, fastmcp_numbers)?,
                opt_bool(args, "require_self", false)?,
            )
        }
        "kakaotalk_ax_send_result" => {
            check_allowed_keys(args, &["operation_id", "wait_seconds"], allow_extra)?;
            backend.send_result(
                req_str(args, "operation_id")?,
                opt_f64(args, "wait_seconds", 0.0, fastmcp_numbers)?,
            )
        }
        _ => Err(KaxError::validation(format!("Unknown tool: {tool_name}"))),
    }
}
