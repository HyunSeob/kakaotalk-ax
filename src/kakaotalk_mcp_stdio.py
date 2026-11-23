"""The local MCP tools over JSON-RPC stdio using only the standard library."""

import asyncio
import base64
from dataclasses import asdict, dataclass
import inspect
import json
from pathlib import Path
import sys
from types import SimpleNamespace, UnionType
from typing import get_args, get_origin


VERSIONS = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
MAX_LINE = 16 * 1024 * 1024


@dataclass(frozen=True)
class ToolAnnotations:
    readOnlyHint: bool
    destructiveHint: bool
    idempotentHint: bool
    openWorldHint: bool


@dataclass(frozen=True)
class TextContent:
    type: str
    text: str


@dataclass(frozen=True)
class ImageContent:
    type: str
    data: str
    mimeType: str


class Image:
    def __init__(self, path):
        self.path = Path(path)

    def to_image_content(self):
        return ImageContent("image", base64.b64encode(self.path.read_bytes()).decode("ascii"), "image/png")


def schema(annotation):
    kind = get_origin(annotation)
    if kind is list:
        return {"type": "array", "items": schema(get_args(annotation)[0])}
    if kind is UnionType:
        return {"anyOf": [schema(a) for a in get_args(annotation)]}
    return {"type": {str: "string", int: "integer", float: "number", bool: "boolean", type(None): "null"}[annotation]}


def valid(value, annotation):
    kind = get_origin(annotation)
    if kind is list:
        return type(value) is list and all(valid(v, get_args(annotation)[0]) for v in value)
    if kind is UnionType:
        return any(valid(value, a) for a in get_args(annotation))
    return type(value) in ((int, float) if annotation is float else (annotation,))


@dataclass(frozen=True)
class Tool:
    name: str
    description: str
    inputSchema: dict
    annotations: ToolAnnotations


class StdioMCP:
    def __init__(self, name, instructions):
        self.name = name
        self.instructions = instructions
        self.tools = {}

    def tool(self, *, annotations):
        def register(function):
            fields = inspect.signature(function).parameters
            properties = {}
            required = []
            for name, field in fields.items():
                properties[name] = schema(field.annotation)
                if field.default is inspect.Parameter.empty:
                    required.append(name)
                else:
                    properties[name]["default"] = field.default
            tool = Tool(function.__name__, inspect.getdoc(function) or "",
                        {"type": "object", "properties": properties, "required": required,
                         "additionalProperties": False}, annotations)
            self.tools[tool.name] = (tool, function)
            return function
        return register

    async def list_tools(self):
        return [tool for tool, _ in self.tools.values()]

    def call_tool(self, params):
        name, args = params.get("name"), params.get("arguments", {})
        if not isinstance(name, str) or name not in self.tools or not isinstance(args, dict):
            raise RpcError(-32602, "Unknown tool or invalid arguments")
        _, function = self.tools[name]
        sig = inspect.signature(function)
        try:
            sig.bind(**args)
            if not all(valid(value, sig.parameters[key].annotation) for key, value in args.items()):
                raise TypeError("Argument type does not match the tool schema")
            result = function(**args)
        except (TypeError, ValueError) as error:
            return {"isError": True, "content": [asdict(TextContent("text", str(error)))]}
        except Exception as error:
            return {"isError": True, "content": [asdict(TextContent("text", type(error).__name__))]}
        content = [TextContent("text", result)] if isinstance(result, str) else result
        return {"isError": False, "content": [asdict(part) for part in content]}

    def run(self, transport="stdio"):
        if transport != "stdio":
            raise ValueError("KakaoTalk MCP supports stdio")
        self.serve(sys.stdin.buffer, sys.stdout.buffer)

    def serve(self, reader, writer):
        initialized = ready = False
        while line := reader.readline(MAX_LINE + 1):
            request_id = None
            try:
                if len(line) > MAX_LINE:
                    return
                request = json.loads(line)
                if not isinstance(request, dict) or request.get("jsonrpc") != "2.0":
                    raise RpcError(-32600, "Invalid Request")
                method, params = request.get("method"), request.get("params", {})
                if not isinstance(method, str) or not isinstance(params, dict):
                    raise RpcError(-32600, "Invalid Request")
                if "id" not in request:
                    if method == "notifications/initialized" and initialized:
                        ready = True
                    continue
                request_id = request["id"]
                if type(request_id) not in (str, int):
                    request_id = None
                    raise RpcError(-32600, "Invalid Request")
                if method == "ping":
                    result = {}
                elif method == "initialize" and not initialized:
                    version = params.get("protocolVersion")
                    if not isinstance(version, str):
                        raise RpcError(-32602, "protocolVersion is required")
                    initialized = True
                    result = {"protocolVersion": version if version in VERSIONS else VERSIONS[0],
                              "capabilities": {"tools": {}}, "serverInfo": {"name": self.name, "version": "0.9.0"},
                              "instructions": self.instructions}
                elif not ready:
                    raise RpcError(-32002, "Server not initialized")
                elif method == "tools/list":
                    result = {"tools": [asdict(tool) for tool, _ in self.tools.values()]}
                elif method == "tools/call":
                    result = self.call_tool(params)
                else:
                    raise RpcError(-32601, "Method not found")
                payload = {"result": result}
            except (UnicodeDecodeError, json.JSONDecodeError):
                payload = {"error": {"code": -32700, "message": "Parse error"}}
            except RpcError as error:
                payload = {"error": {"code": error.code, "message": str(error)}}
            writer.write(wire({"jsonrpc": "2.0", "id": request_id, **payload}))
            writer.flush()


class RpcError(RuntimeError):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def wire(message):
    return (json.dumps(message, ensure_ascii=False, allow_nan=False) + "\n").encode("utf-8")


class StdioClient:
    def __init__(self, process, timeout):
        self.process = process
        self.timeout = timeout
        self.sequence = 0

    async def send(self, method, params=None, notification=False):
        request = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            request["params"] = params
        if not notification:
            self.sequence += 1
            request["id"] = self.sequence
        self.process.stdin.write(wire(request))
        await self.process.stdin.drain()
        if notification:
            return None
        raw = await asyncio.wait_for(self.process.stdout.readline(), self.timeout)
        if not raw:
            raise RuntimeError("KakaoTalk MCP closed its output")
        response = json.loads(raw)
        if response.get("id") != self.sequence or "error" in response:
            raise RuntimeError("Invalid KakaoTalk MCP reply")
        return response["result"]

    async def initialize(self):
        reply = await self.send("initialize", {"protocolVersion": VERSIONS[0], "capabilities": {},
                                               "clientInfo": {"name": "kax", "version": "0.9.0"}})
        if reply.get("protocolVersion") not in VERSIONS:
            raise RuntimeError("Unsupported KakaoTalk MCP protocol version")
        await self.send("notifications/initialized", notification=True)

    async def call_tool(self, name, args):
        result = await self.send("tools/call", {"name": name, "arguments": args})
        return SimpleNamespace(isError=result.get("isError", False),
                               content=[SimpleNamespace(**part) for part in result["content"]])
