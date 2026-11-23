from io import BytesIO
import json
import unittest

from kakaotalk_mcp_stdio import StdioMCP, ToolAnnotations, wire


class StdlibMCPTests(unittest.TestCase):
    def setUp(self):
        self.server = StdioMCP("fixture", "instructions")
        self.calls = []

        @self.server.tool(annotations=ToolAnnotations(False, False, False, True))
        def submit(window_title: str, operation_id: str, count: int = 1, output_path: str | None = None) -> str:
            self.calls.append((window_title, operation_id, count, output_path))
            return "한글 🧪\n둘째 줄"

    def session(self, *messages, handshake=True):
        request = [] if not handshake else [
            {"id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                                                        "clientInfo": {"name": "test", "version": "1"}}},
            {"method": "notifications/initialized"}]
        request.extend(messages)
        incoming = BytesIO(b"".join(wire({"jsonrpc": "2.0", **entry}) for entry in request))
        outgoing = BytesIO()
        self.server.serve(incoming, outgoing)
        return [json.loads(line) for line in outgoing.getvalue().splitlines()]

    def test_lifecycle_schemas_annotations_and_utf8_content(self):
        messages = self.session({"id": 2, "method": "tools/list"}, {"id": 3, "method": "tools/call",
                                "params": {"name": "submit", "arguments": {"window_title": "내 방", "operation_id": "id"}}})
        self.assertEqual(messages[0]["result"]["protocolVersion"], "2025-11-25")
        tool = messages[1]["result"]["tools"][0]
        self.assertEqual(tool["inputSchema"]["required"], ["window_title", "operation_id"])
        self.assertFalse(tool["annotations"]["idempotentHint"])
        self.assertEqual(messages[2]["result"]["content"], [{"type": "text", "text": "한글 🧪\n둘째 줄"}])
        self.assertEqual(len(self.calls), 1)

    def test_tools_are_gated_by_initialization_and_arguments_are_validated(self):
        unready = self.session({"id": 11, "method": "tools/list"}, handshake=False)
        self.assertEqual(unready[0]["error"]["code"], -32002)
        result = self.session({"id": 2, "method": "tools/call", "params": {"name": "submit",
                              "arguments": {"window_title": "내 방", "operation_id": "id", "count": True}}})
        self.assertTrue(result[-1]["result"]["isError"])
        self.assertEqual(self.calls, [])


if __name__ == "__main__":
    unittest.main()
