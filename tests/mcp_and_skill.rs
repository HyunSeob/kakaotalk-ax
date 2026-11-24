use std::collections::HashSet;
use std::io::Cursor;

use serde_json::{json, Value};

use kakaotalk_ax::ax_core::KakaoBackend;
use kakaotalk_ax::error::Result;
use kakaotalk_ax::mcp::{tool_definitions, tool_definitions_for, McpProtocolStyle, McpServer};

use std::sync::atomic::{AtomicUsize, Ordering};

struct MockMcpBackend {
    attachment_path: Option<String>,
    attachment_status: String,
    fail_send: bool,
    windows_calls: AtomicUsize,
    read_calls: AtomicUsize,
    send_calls: AtomicUsize,
}

impl MockMcpBackend {
    fn new() -> Self {
        Self {
            attachment_path: None,
            attachment_status: "ok".to_string(),
            fail_send: false,
            windows_calls: AtomicUsize::new(0),
            read_calls: AtomicUsize::new(0),
            send_calls: AtomicUsize::new(0),
        }
    }
}

impl KakaoBackend for MockMcpBackend {
    fn build(&self, kind: &str) -> Result<Value> {
        Ok(json!({"status": "built", "kind": kind}))
    }
    fn windows(&self) -> Result<Value> {
        self.windows_calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"status": "ok", "items": []}))
    }
    fn rooms(&self, query: &str) -> Result<Value> {
        Ok(json!({"status": "ok", "query": query, "items": []}))
    }
    fn search_rooms(&self, query: &str) -> Result<Value> {
        Ok(json!({"status": "ok", "query": query, "items": []}))
    }
    fn open_room(&self, window_title: &str, _require_self: bool) -> Result<Value> {
        Ok(json!({"status": "ok", "windowTitle": window_title}))
    }
    fn close_room(&self, window_title: &str) -> Result<Value> {
        Ok(json!({"status": "ok", "windowTitle": window_title}))
    }
    fn read_messages(&self, window_title: &str, limit: i64, offset: i64, _snap: &str) -> Result<Value> {
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        let large_id: u64 = (1u64 << 60) + 123;
        Ok(json!({
            "status": "ok",
            "windowTitle": window_title,
            "limit": limit,
            "offset": offset,
            "hasMore": true,
            "nextOffset": offset + limit,
            "items": [{"logId": large_id, "text": "한글 🧪\n둘째 줄"}],
            "databaseCalls": 0,
        }))
    }
    fn read_attachment(
        &self,
        _window_title: &str,
        _row_index: i64,
        _snapshot_id: &str,
        _output_path: Option<&str>,
        ocr: bool,
    ) -> Result<Value> {
        if self.attachment_status != "ok" {
            return Ok(json!({"status": self.attachment_status}));
        }
        Ok(json!({
            "status": "ok",
            "imagePath": self.attachment_path,
            "ocrPerformed": ocr,
        }))
    }
    fn read_many(&self, _t: &[String], _l: i64, _o: bool, _c: bool, _w: i64) -> Result<Value> {
        Ok(json!({"status": "ok", "items": []}))
    }
    fn load_more(&self, _wt: &str, _p: i64) -> Result<Value> {
        Ok(json!({"status": "ok"}))
    }
    fn search_messages(&self, _wt: &str, _q: &str, _l: i64, _lp: i64, _cs: bool, _u: bool, _ms: i64, _cr: i64) -> Result<Value> {
        Ok(json!({"status": "ok", "items": []}))
    }
    fn close_search(&self, _wt: &str) -> Result<Value> {
        Ok(json!({"status": "ok"}))
    }
    fn send_message(&self, wt: &str, _t: &str, op: &str, _w: f64, _rs: bool) -> Result<Value> {
        self.send_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_send {
            return Err(kakaotalk_ax::error::KaxError::runtime("RuntimeError", "lost reply"));
        }
        Ok(json!({"status": "observed_in_ax", "windowTitle": wt, "operationId": op}))
    }
    fn send_result(&self, op: &str, _w: f64) -> Result<Value> {
        Ok(json!({"status": "observed_in_ax", "operationId": op}))
    }
}

fn run_session(backend: &dyn KakaoBackend, messages: &[Value], handshake: bool) -> Vec<Value> {
    let mut all = Vec::new();
    if handshake {
        all.push(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"}
            }
        }));
        all.push(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }
    for m in messages {
        let mut obj = m.as_object().unwrap().clone();
        obj.insert("jsonrpc".to_string(), json!("2.0"));
        all.push(Value::Object(obj));
    }
    let input: String = all
        .iter()
        .map(|v| format!("{}\n", serde_json::to_string(v).unwrap()))
        .collect();
    let mut output = Vec::new();
    McpServer::new(backend)
        .serve(Cursor::new(input.into_bytes()), &mut output)
        .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn server_exposes_exact_13_tools_and_annotations() {
    let tools = tool_definitions();
    assert_eq!(tools.len(), 13);
    let expected: HashSet<&str> = [
        "kakaotalk_ax_windows",
        "kakaotalk_ax_read",
        "kakaotalk_ax_read_attachment",
        "kakaotalk_ax_rooms",
        "kakaotalk_ax_search_rooms",
        "kakaotalk_ax_open",
        "kakaotalk_ax_close",
        "kakaotalk_ax_read_many",
        "kakaotalk_ax_load_more",
        "kakaotalk_ax_search",
        "kakaotalk_ax_close_search",
        "kakaotalk_ax_send",
        "kakaotalk_ax_send_result",
    ]
    .into_iter()
    .collect();
    let actual: HashSet<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(actual, expected);

    let send_tool = tools
        .iter()
        .find(|t| t["name"] == "kakaotalk_ax_send")
        .unwrap();
    let req = send_tool["inputSchema"]["required"].as_array().unwrap();
    assert!(req.contains(&json!("operation_id")));
    assert_eq!(send_tool["annotations"]["idempotentHint"], false);
    assert_eq!(send_tool["annotations"]["readOnlyHint"], false);

    let read_only: HashSet<&str> = tools
        .iter()
        .filter(|t| t["annotations"]["readOnlyHint"] == true)
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        read_only,
        HashSet::from([
            "kakaotalk_ax_windows",
            "kakaotalk_ax_read",
            "kakaotalk_ax_send_result",
        ])
    );

    // macOS FastMCP profile exposes outputSchema and inputSchema.title on all 13 tools
    for tool in tool_definitions_for(McpProtocolStyle::MacFastMcp) {
        let name = tool["name"].as_str().unwrap();
        assert!(
            tool["inputSchema"]["title"].is_string(),
            "missing inputSchema.title on {name}"
        );
        let out = &tool["outputSchema"];
        assert_eq!(out["type"], "object", "missing outputSchema on {name}");
        assert_eq!(out["required"], json!(["result"]));
        if name == "kakaotalk_ax_read_attachment" {
            assert_eq!(out["properties"]["result"]["type"], "array");
            assert!(out["$defs"]["TextContent"].is_object());
            assert!(out["$defs"]["ImageContent"].is_object());
        } else {
            assert_eq!(out["properties"]["result"]["type"], "string");
        }
    }

    // Windows StdioMCP profile matches e0127d0 kakaotalk_mcp_stdio.py (no outputSchema/title, additionalProperties: false)
    for tool in tool_definitions_for(McpProtocolStyle::WindowsStdio) {
        let name = tool["name"].as_str().unwrap();
        assert!(tool.get("outputSchema").is_none(), "unexpected outputSchema on {name}");
        assert!(tool["inputSchema"].get("title").is_none(), "unexpected inputSchema.title on {name}");
        assert_eq!(tool["inputSchema"]["additionalProperties"], json!(false));
        assert!(tool["inputSchema"]["required"].is_array());
        assert!(!tool["description"].as_str().unwrap().ends_with('\n'));
    }
}

#[test]
fn lifecycle_schemas_annotations_and_utf8_content() {
    let backend = MockMcpBackend::new();
    let replies = run_session(
        &backend,
        &[
            json!({"id": 2, "method": "tools/list"}),
            json!({
                "id": 3,
                "method": "tools/call",
                "params": {"name": "kakaotalk_ax_read", "arguments": {"window_title": "내 방", "limit": 2}}
            }),
        ],
        true,
    );
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-11-25");
    let listed = replies[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(listed.len(), 13);
    let call_res = &replies[2]["result"];
    assert_eq!(call_res["isError"], false);
    if McpProtocolStyle::platform_default() == McpProtocolStyle::MacFastMcp {
        assert!(listed.iter().all(|t| t["outputSchema"].is_object()));
        assert!(call_res["structuredContent"]["result"].is_string());
    } else {
        assert!(listed.iter().all(|t| t.get("outputSchema").is_none()));
        assert!(call_res.get("structuredContent").is_none());
    }
    let body: Value = serde_json::from_str(call_res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["windowTitle"], "내 방");
    assert_eq!(body["limit"], 2);
    assert_eq!(body["items"][0]["text"], "한글 🧪\n둘째 줄");
}

#[test]
fn tools_are_gated_by_initialization_and_arguments_are_validated() {
    let backend = MockMcpBackend::new();
    let unready = run_session(&backend, &[json!({"id": 11, "method": "tools/list"})], false);
    assert_eq!(unready[0]["error"]["code"], -32002);

    // Passing boolean where integer is expected fails with isError: true
    let replies = run_session(
        &backend,
        &[json!({
            "id": 2,
            "method": "tools/call",
            "params": {"name": "kakaotalk_ax_read", "arguments": {"window_title": "내 방", "limit": true}}
        })],
        true,
    );
    assert_eq!(replies.last().unwrap()["result"]["isError"], true);

    // Unknown/legacy tool: MacFastMcp returns result.isError=true; WindowsStdio returns JSON-RPC -32602
    let unknown = run_session(
        &backend,
        &[json!({
            "id": 3,
            "method": "tools/call",
            "params": {"name": "kakaotalk_send_self", "arguments": {}}
        })],
        true,
    );
    if McpProtocolStyle::platform_default() == McpProtocolStyle::MacFastMcp {
        assert_eq!(unknown.last().unwrap()["result"]["isError"], true);
        assert!(unknown.last().unwrap()["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"));
    } else {
        assert_eq!(unknown.last().unwrap()["error"]["code"], -32602);
    }
}

#[test]
fn old_sender_ui_and_unknown_tools_cannot_be_requested() {
    let backend = MockMcpBackend::new();
    for legacy in [
        "kakaotalk_send_self",
        "kakaotalk_ui_read",
        "kakaotalk_status",
        "kakaotalk_rooms",
        "kakaotalk_read",
        "kakaotalk_sql",
        "kakaotalk_send",
        "kakaotalk_send_result",
        "kakaotalk_get_media",
        "unknown",
    ] {
        let replies = run_session(
            &backend,
            &[json!({
                "id": 2,
                "method": "tools/call",
                "params": {"name": legacy, "arguments": {}}
            })],
            true,
        );
        let last = replies.last().unwrap();
        if McpProtocolStyle::platform_default() == McpProtocolStyle::MacFastMcp {
            assert_eq!(last["result"]["isError"], true, "tool={legacy}");
        } else {
            assert_eq!(last["error"]["code"], -32602, "tool={legacy}");
        }
    }
}

#[test]
fn text_result_keeps_pagination_and_large_ids_exact() {
    let backend = MockMcpBackend::new();
    let replies = run_session(
        &backend,
        &[json!({
            "id": 2,
            "method": "tools/call",
            "params": {"name": "kakaotalk_ax_read", "arguments": {"window_title": "Fixture", "limit": 50, "offset": 50}}
        })],
        true,
    );
    let text = replies[1]["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["windowTitle"], "Fixture");
    assert_eq!(parsed["databaseCalls"], 0);
    assert_eq!(parsed["hasMore"], true);
    assert_eq!(parsed["nextOffset"], 100);
    let expected_id: u64 = (1u64 << 60) + 123;
    assert_eq!(parsed["items"][0]["logId"].as_u64(), Some(expected_id));
}

#[test]
fn ping_and_unknown_method_and_parse_error_follow_jsonrpc() {
    let backend = MockMcpBackend::new();
    let replies = run_session(
        &backend,
        &[
            json!({"id": 2, "method": "ping"}),
            json!({"id": 3, "method": "nonexistent/method"}),
        ],
        true,
    );
    assert_eq!(replies[1]["result"], json!({}));
    assert_eq!(replies[2]["error"]["code"], -32601);

    let mut out = Vec::new();
    McpServer::new(&backend)
        .serve(Cursor::new(b"{not-json\n".to_vec()), &mut out)
        .unwrap();
    let err: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(err["error"]["code"], -32700);
}

#[test]
fn cli_send_message_validation_error_preserves_outcome_unknown_and_operation_id() {
    let dir = tempfile::tempdir().unwrap();
    let args_path = dir.path().join("send_bad.json");
    std::fs::write(
        &args_path,
        r#"{"window_title": " ", "text": "hello", "operation_id": "ax-intent-err"}"#,
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kax"))
        .args(["ax-call", "send_message", "--args-file"])
        .arg(&args_path)
        .output()
        .expect("spawn kax");
    assert_eq!(output.status.code(), Some(1));
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(parsed["status"], "outcome_unknown");
    assert_eq!(parsed["operationId"], "ax-intent-err");
    assert_eq!(parsed["retrySafe"], false);
    assert_eq!(parsed["errorType"], "ValueError");
}

#[test]
fn attachment_tool_returns_image_content_on_ok_and_no_image_on_snapshot_changed() {
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("fixture.png");
    std::fs::write(&img_path, b"\x89PNG\r\n\x1a\n").unwrap();

    let mut backend = MockMcpBackend::new();
    backend.attachment_path = Some(img_path.display().to_string());
    let ok_replies = run_session(
        &backend,
        &[json!({
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "kakaotalk_ax_read_attachment",
                "arguments": {"window_title": "Fixture", "row_index": 4, "snapshot_id": "a".repeat(24)}
            }
        })],
        true,
    );
    let parts = ok_replies[1]["result"]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mimeType"], "image/png");

    backend.attachment_status = "snapshot_changed".to_string();
    let blocked_replies = run_session(
        &backend,
        &[json!({
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "kakaotalk_ax_read_attachment",
                "arguments": {"window_title": "Fixture", "row_index": 4, "snapshot_id": "a".repeat(24)}
            }
        })],
        true,
    );
    let parts2 = blocked_replies[1]["result"]["content"].as_array().unwrap();
    assert_eq!(parts2.len(), 1);
    assert_eq!(parts2[0]["type"], "text");
}

#[test]
fn windows_stdio_profile_omits_structured_content_while_macos_fastmcp_includes_it() {
    let backend = MockMcpBackend::new();
    let reqs = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "kakaotalk_ax_windows", "arguments": {}}
        }),
    ];
    let wire: String = reqs.iter().map(|v| format!("{}\n", serde_json::to_string(v).unwrap())).collect();

    let mut win_out = Vec::new();
    McpServer::with_style(&backend, McpProtocolStyle::WindowsStdio)
        .serve(Cursor::new(wire.as_bytes()), &mut win_out)
        .unwrap();
    let win_replies: Vec<Value> = String::from_utf8(win_out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(win_replies[1]["result"]["isError"], false);
    assert!(win_replies[1]["result"].get("structuredContent").is_none());

    let mut mac_out = Vec::new();
    McpServer::with_style(&backend, McpProtocolStyle::MacFastMcp)
        .serve(Cursor::new(wire.as_bytes()), &mut mac_out)
        .unwrap();
    let mac_replies: Vec<Value> = String::from_utf8(mac_out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(mac_replies[1]["result"]["isError"], false);
    assert!(mac_replies[1]["result"]["structuredContent"]["result"].is_string());

    // MacFastMcp ignores extra arguments (Pydantic default) and returns result.isError=true for unknown tools;
    // WindowsStdio rejects extra arguments and returns JSON-RPC -32602 for unknown tools.
    let extra_and_unknown = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "kakaotalk_ax_windows", "arguments": {"unused": 1}}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "kakaotalk_missing", "arguments": {}}
        }),
    ];
    let wire2: String = extra_and_unknown
        .iter()
        .map(|v| format!("{}\n", serde_json::to_string(v).unwrap()))
        .collect();

    let mut mac_out2 = Vec::new();
    McpServer::with_style(&backend, McpProtocolStyle::MacFastMcp)
        .serve(Cursor::new(wire2.as_bytes()), &mut mac_out2)
        .unwrap();
    let mac_r2: Vec<Value> = String::from_utf8(mac_out2)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(mac_r2[1]["result"]["isError"], false);
    assert_eq!(mac_r2[2]["result"]["isError"], true);
    assert_eq!(
        mac_r2[2]["result"]["content"][0]["text"],
        "Unknown tool: kakaotalk_missing"
    );

    let mut win_out2 = Vec::new();
    McpServer::with_style(&backend, McpProtocolStyle::WindowsStdio)
        .serve(Cursor::new(wire2.as_bytes()), &mut win_out2)
        .unwrap();
    let win_r2: Vec<Value> = String::from_utf8(win_out2)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(win_r2[1]["result"]["isError"], true);
    assert_eq!(win_r2[2]["error"]["code"], -32602);

    // Integral JSON floats (1.0, 1e0) are accepted by MacFastMcp (JSON Schema integer / Pydantic),
    // rejected by WindowsStdio and CLI dispatch_method; fractional 1.5 is rejected everywhere.
    let num_wire = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#, "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"kakaotalk_ax_read","arguments":{"window_title":"Fixture","limit":1.0}}}"#, "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"kakaotalk_ax_read","arguments":{"window_title":"Fixture","limit":1e0}}}"#, "\n",
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"kakaotalk_ax_read","arguments":{"window_title":"Fixture","limit":1.5}}}"#, "\n",
    );

    let mut mac_num_out = Vec::new();
    McpServer::with_style(&backend, McpProtocolStyle::MacFastMcp)
        .serve(Cursor::new(num_wire.as_bytes()), &mut mac_num_out)
        .unwrap();
    let mac_num: Vec<Value> = String::from_utf8(mac_num_out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(mac_num[1]["result"]["isError"], false);
    assert_eq!(mac_num[2]["result"]["isError"], false);
    assert_eq!(mac_num[3]["result"]["isError"], true);

    let mut win_num_out = Vec::new();
    McpServer::with_style(&backend, McpProtocolStyle::WindowsStdio)
        .serve(Cursor::new(num_wire.as_bytes()), &mut win_num_out)
        .unwrap();
    let win_num: Vec<Value> = String::from_utf8(win_num_out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(win_num[1]["result"]["isError"], true);
    assert_eq!(win_num[2]["result"]["isError"], true);
    assert_eq!(win_num[3]["result"]["isError"], true);

    // CLI dispatch_method also continues to reject 1.0 as non-integer.
    let cli_args: serde_json::Map<String, Value> =
        serde_json::from_str(r#"{"window_title":"Fixture","limit":1.0}"#).unwrap();
    assert!(kakaotalk_ax::mcp::dispatch_method(&backend, "read_messages", &cli_args).is_err());
}

#[test]
fn ax_tools_delegate_exact_call_counts_and_send_error_is_never_retried() {
    let mut backend = MockMcpBackend::new();
    backend.fail_send = true;
    let replies = run_session(
        &backend,
        &[
            json!({"id": 2, "method": "tools/call", "params": {"name": "kakaotalk_ax_windows", "arguments": {}}}),
            json!({"id": 3, "method": "tools/call", "params": {"name": "kakaotalk_ax_read", "arguments": {"window_title": "Fixture", "limit": 5}}}),
            json!({"id": 4, "method": "tools/call", "params": {"name": "kakaotalk_ax_send", "arguments": {"window_title": "Fixture", "text": "hi", "operation_id": "op-lost-1"}}}),
        ],
        true,
    );
    assert_eq!(replies[1]["result"]["isError"], false);
    assert_eq!(replies[2]["result"]["isError"], false);
    assert_eq!(replies[3]["result"]["isError"], true);
    assert_eq!(backend.windows_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.read_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.send_calls.load(Ordering::SeqCst), 1);
}
