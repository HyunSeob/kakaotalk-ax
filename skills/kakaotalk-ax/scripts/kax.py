#!/usr/bin/env python3
"""Invoke the local KakaoTalk MCP server without a persistent connector registration."""

import argparse
import asyncio
import json
import os
import subprocess
import sys
from datetime import timedelta
from pathlib import Path


SOURCE_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_ROOT = (SOURCE_ROOT if (SOURCE_ROOT / "pyproject.toml").is_file() else
                Path.home() / "Documents/workspace/kakaotalk-ax")
READ_ONLY_TOOLS = frozenset({"kakaotalk_ax_windows", "kakaotalk_ax_read", "kakaotalk_ax_send_result"})
SEND_TOOL = "kakaotalk_ax_send"
SEND_TOOLS = frozenset({SEND_TOOL})
UI_TOOLS = frozenset({"kakaotalk_ax_rooms", "kakaotalk_ax_search_rooms", "kakaotalk_ax_open",
                      "kakaotalk_ax_close", "kakaotalk_ax_read_many", "kakaotalk_ax_load_more",
                      "kakaotalk_ax_search", "kakaotalk_ax_close_search", "kakaotalk_ax_read_attachment"})


def project_python(root):
    return root / (".venv/Scripts/python.exe" if sys.platform == "win32" else ".venv/bin/python")


def checked_arguments(name, arguments):
    if not isinstance(arguments, dict):
        raise ValueError("Tool arguments must be a JSON object")
    if name is not None and name not in READ_ONLY_TOOLS | SEND_TOOLS | UI_TOOLS:
        raise ValueError("Unsupported KakaoTalk tool")
    return arguments


def render_result(result):
    texts = []
    has_image = False
    for part in result.content:
        if part.type == "image":
            has_image = True
            continue
        if part.type != "text":
            raise ValueError("Unsupported non-text AX result")
        try:
            texts.append(json.loads(part.text))
        except ValueError:
            texts.append(part.text)
    value = texts[0] if len(texts) == 1 else {"content": texts}
    if has_image and (not isinstance(value, dict) or not value.get("imagePath")):
        raise ValueError("Image result has no local imagePath")
    if result.isError:
        return {"status": "mcp_error", "detail": value}
    return value


async def call_once(session, name, arguments):
    checked_arguments(name, arguments)
    result = await session.call_tool(name, arguments)
    value = render_result(result)
    if name in SEND_TOOLS and result.isError:
        return {"status": "outcome_unknown", "operationId": arguments.get("operation_id"),
                "retrySafe": False, "detail": value}
    return value


async def invoke(root, name, arguments, timeout):
    if sys.platform == "win32":
        from kakaotalk_mcp_stdio import MAX_LINE, StdioClient

        process = await asyncio.create_subprocess_exec(
            str(project_python(root)), "-m", "kakaotalk_automation", "mcp",
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, limit=MAX_LINE)
        session = StdioClient(process, timeout)
        try:
            await session.initialize()
            available = (await session.send("tools/list"))["tools"]
            tools = {tool["name"]: tool for tool in available if
                     (tool["name"] in READ_ONLY_TOOLS and tool["annotations"]["readOnlyHint"] is True) or
                     (tool["name"] in SEND_TOOLS | UI_TOOLS and tool["annotations"]["readOnlyHint"] is False)}
            if name is None:
                return {"tools": list(tools.values())}
            if name not in tools:
                raise ValueError("Requested tool is not exposed by this server")
            return await call_once(session, name, arguments)
        finally:
            process.stdin.close()
            if process.returncode is None:
                # EOF lets an in-flight UI operation finish restoring the desktop.
                try:
                    await asyncio.wait_for(process.stdout.read(), 1)
                    await process.wait()
                except asyncio.TimeoutError:
                    pass

    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client

    parameters = StdioServerParameters(command=str(project_python(root)), args=["-m", "kakaotalk_automation", "mcp"])
    async with stdio_client(parameters) as (read, write):
        async with ClientSession(read, write, read_timeout_seconds=timedelta(seconds=timeout)) as session:
            await session.initialize()
            available = await session.list_tools()
            tools = {tool.name: tool for tool in available.tools if tool.annotations is not None and
                     ((tool.name in READ_ONLY_TOOLS and tool.annotations.readOnlyHint is True) or
                      (tool.name in SEND_TOOLS | UI_TOOLS and tool.annotations.readOnlyHint is False))}
            if name is None:
                return {"tools": [tool.model_dump(mode="json", exclude_none=True) for tool in tools.values()]}
            if name not in tools:
                raise ValueError("Requested tool is not exposed by this server")
            return await call_once(session, name, arguments)


def main():
    if sys.platform == "win32" and hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(os.environ.get("KAKAOTALK_AX_ROOT", DEFAULT_ROOT)))
    parser.add_argument("--timeout", type=int, default=75)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("status")
    commands.add_parser("tools")
    call = commands.add_parser("call")
    call.add_argument("tool")
    call.add_argument("--args-file", type=Path)
    args = parser.parse_args()
    root = args.root.expanduser().absolute()
    python = project_python(root)
    if not python.is_file():
        print(json.dumps({"status": "setup_required", "projectRoot": str(root), "detail": "Project .venv is missing"}))
        return 1
    if Path(sys.prefix).resolve() != (root / ".venv").resolve():
        argv = [str(python), str(Path(__file__).absolute()), *sys.argv[1:]]
        if sys.platform == "win32":
            return subprocess.call(argv)
        os.execv(str(python), argv)
    sending_operation = None
    try:
        if not 1 <= args.timeout <= 300:
            raise ValueError("timeout must be between 1 and 300 seconds")
        if args.command == "status":
            name, arguments = "kakaotalk_ax_windows", {}
        elif args.command == "tools":
            name, arguments = None, {}
        else:
            name = args.tool
            arguments = json.loads(args.args_file.read_text(encoding="utf-8")) if args.args_file else {}
        checked_arguments(name, arguments)
        if name in SEND_TOOLS:
            sending_operation = arguments.get("operation_id")
        value = asyncio.run(invoke(root, name, arguments, args.timeout))
        print(json.dumps(value, ensure_ascii=False, indent=2))
        return int(isinstance(value, dict) and value.get("status") in
                   ("mcp_error", "outcome_unknown", "blocked", "failed", "snapshot_changed", "bridge_error"))
    except (Exception, KeyboardInterrupt) as error:
        value = {"status": "outcome_unknown" if sending_operation else "failed",
                 "errorType": type(error).__name__}
        if sending_operation:
            value.update(operationId=sending_operation, retrySafe=False)
        print(json.dumps(value, ensure_ascii=False))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
