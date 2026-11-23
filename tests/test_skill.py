import importlib.util
import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

import kakaotalk_mcp

SCRIPT = Path(__file__).resolve().parents[1] / "skills/kakaotalk-ax/scripts/kax.py"
spec = importlib.util.spec_from_file_location("kakaotalk_skill_client", SCRIPT)
client = importlib.util.module_from_spec(spec)
spec.loader.exec_module(client)


class SkillClientTests(unittest.TestCase):
    def test_old_sender_ui_and_unknown_tools_cannot_be_requested(self):
        for tool in ("kakaotalk_send_self", "kakaotalk_ui_read", "kakaotalk_status",
                     "kakaotalk_rooms", "kakaotalk_read", "kakaotalk_sql", "kakaotalk_send",
                     "kakaotalk_send_result", "kakaotalk_get_media", "unknown"):
            with self.subTest(tool=tool), self.assertRaises(ValueError):
                client.checked_arguments(tool, {})
        with self.assertRaises(ValueError):
            client.checked_arguments("kakaotalk_read", {})
        with self.assertRaises(ValueError):
            client.checked_arguments("kakaotalk_ax_windows", [])

    def test_text_result_keeps_pagination_and_large_ids_exact(self):
        value = {"items": [{"logId": 2**60 + 123}], "hasMore": True, "nextOffset": 100}
        response = SimpleNamespace(isError=False, content=[SimpleNamespace(type="text", text=json.dumps(value))])
        self.assertEqual(client.render_result(response), value)

    def test_non_text_content_is_rejected_without_writing_files(self):
        response = SimpleNamespace(isError=False, content=[SimpleNamespace(type="image")])
        with self.assertRaises(ValueError):
            client.render_result(response)

    def test_attachment_image_keeps_local_path_without_dumping_base64(self):
        value = {"status": "ok", "imagePath": "/tmp/fixture.png", "ocrPerformed": False}
        response = SimpleNamespace(isError=False, content=[
            SimpleNamespace(type="text", text=json.dumps(value)), SimpleNamespace(type="image", data="not-printed")])
        self.assertEqual(client.render_result(response), value)

    def test_read_error_is_reported(self):
        response = SimpleNamespace(isError=True, content=[SimpleNamespace(type="text", text="tool failed")])
        self.assertEqual(client.render_result(response)["status"], "mcp_error")


class SkillInvocationTests(unittest.IsolatedAsyncioTestCase):
    async def test_read_timeout_is_not_retried(self):
        session = SimpleNamespace(call_tool=AsyncMock(side_effect=TimeoutError()))
        with self.assertRaises(TimeoutError):
            await client.call_once(session, "kakaotalk_ax_windows", {})
        session.call_tool.assert_awaited_once()

    async def test_blocked_tool_never_reaches_mcp(self):
        session = SimpleNamespace(call_tool=AsyncMock())
        for name in ("kakaotalk_ui_read", "unknown"):
            with self.subTest(tool=name), self.assertRaises(ValueError):
                await client.call_once(session, name, {})
        session.call_tool.assert_not_awaited()

    async def test_server_exposes_declared_queries_navigation_and_sends(self):
        tools = await kakaotalk_mcp.mcp.list_tools()
        by_name = {tool.name: tool for tool in tools}
        self.assertEqual(set(by_name), client.READ_ONLY_TOOLS | client.SEND_TOOLS | client.UI_TOOLS)
        self.assertEqual(len(by_name), 13)
        writes = {tool.name for tool in tools if not tool.annotations.readOnlyHint}
        self.assertEqual(writes, client.SEND_TOOLS | client.UI_TOOLS)
        for name in client.SEND_TOOLS:
            self.assertIn("operation_id", by_name[name].inputSchema["required"])
            self.assertFalse(by_name[name].annotations.idempotentHint)

    async def test_ax_tools_delegate_to_native_helper(self):
        with patch("kakaotalk_automation.NativeAX") as native:
            native.return_value.windows.return_value = {"status": "ok", "items": []}
            native.return_value.read_messages.return_value = {"status": "ok", "items": [], "databaseCalls": 0}
            self.assertEqual(json.loads(kakaotalk_mcp.kakaotalk_ax_windows())["status"], "ok")
            self.assertEqual(json.loads(kakaotalk_mcp.kakaotalk_ax_read("Fixture", 2))["databaseCalls"], 0)
            native.return_value.read_messages.assert_called_once_with("Fixture", 2, 0, "")

    async def test_attachment_tool_returns_image_content_and_metadata(self):
        from kakaotalk_mcp import ImageContent
        with patch("kakaotalk_automation.NativeAX") as native, patch("kakaotalk_mcp.Image") as image:
            native.return_value.read_attachment.return_value = {
                "status": "ok", "imagePath": "/tmp/fixture.png", "ocrPerformed": False}
            image.return_value.to_image_content.return_value = ImageContent(type="image", data="AA==", mimeType="image/png")
            result = kakaotalk_mcp.kakaotalk_ax_read_attachment("Fixture", 4, "a" * 24)
            self.assertEqual([part.type for part in result], ["text", "image"])
            self.assertFalse(json.loads(result[0].text)["ocrPerformed"])
            native.return_value.read_attachment.assert_called_once_with("Fixture", 4, "a" * 24, None, False)

    async def test_blocked_attachment_tool_returns_no_image(self):
        with patch("kakaotalk_automation.NativeAX") as native:
            native.return_value.read_attachment.return_value = {"status": "snapshot_changed"}
            result = kakaotalk_mcp.kakaotalk_ax_read_attachment("Fixture", 4, "a" * 24)
            self.assertEqual([part.type for part in result], ["text"])

    async def test_send_tool_error_remains_uncertain_with_original_operation_id(self):
        response = SimpleNamespace(isError=True, content=[SimpleNamespace(type="text", text="lost result")])
        session = SimpleNamespace(call_tool=AsyncMock(return_value=response))
        result = await client.call_once(session, client.SEND_TOOL, {"operation_id": "intent-1"})
        self.assertEqual(result["status"], "outcome_unknown")
        self.assertEqual(result["operationId"], "intent-1")
        self.assertFalse(result["retrySafe"])
        session.call_tool.assert_awaited_once()

    async def test_ax_send_error_is_uncertain_and_never_retried(self):
        response = SimpleNamespace(isError=True, content=[SimpleNamespace(type="text", text="lost result")])
        session = SimpleNamespace(call_tool=AsyncMock(return_value=response))
        result = await client.call_once(session, "kakaotalk_ax_send", {"operation_id": "ax-intent"})
        self.assertEqual(result["status"], "outcome_unknown")
        self.assertEqual(result["operationId"], "ax-intent")
        session.call_tool.assert_awaited_once()


if __name__ == "__main__":
    unittest.main()
