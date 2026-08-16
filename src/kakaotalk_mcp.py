#!/usr/bin/env python3
"""Native MCP stdio bridge for the read-only KakaoTalk service.

The bridge intentionally exposes named, bounded operations only.  It does not
expose an arbitrary SQL tool; the CLI's separately guarded SELECT/WITH escape
hatch is not part of Hermes' default tool surface.
"""

from __future__ import annotations

import os
import sys

# Allow direct execution from a source checkout without installing the module.
sys.path.insert(0, os.path.dirname(__file__))

from mcp.server.fastmcp import FastMCP

from kakaotalk_service import (
    DEFAULT_CONTEXT_RADIUS,
    DEFAULT_MESSAGE_LIMIT,
    DEFAULT_ROOM_LIMIT,
    DEFAULT_USER_LIMIT,
    KakaoTalkService,
    ServiceError,
    ValidationError,
    serialize_json,
)


mcp = FastMCP(
    "kakaotalk",
    instructions=(
        "Read-only access to the currently running KakaoTalk Mac local database. "
        "Use the named room, message, search, user, and context tools. "
        "Results are bounded JSON and contain no write operation."
    ),
)
SERVICE = KakaoTalkService()


def _tool_json(operation):
    try:
        return serialize_json(operation())
    except ValidationError:
        raise
    except ServiceError:
        raise
    except Exception as exc:
        # Do not turn an unexpected exception into a path, key, SQL, or row
        # dump in the MCP response or server diagnostics.
        raise ServiceError("KakaoTalk tool failed without exposing database details") from exc


@mcp.tool()
def kakaotalk_list_rooms(limit: int = DEFAULT_ROOM_LIMIT) -> str:
    """List KakaoTalk rooms with stable IDs, type labels, and display names."""

    return _tool_json(lambda: SERVICE.list_rooms(limit=limit))


@mcp.tool()
def kakaotalk_read_messages(room_id: int, limit: int = DEFAULT_MESSAGE_LIMIT) -> str:
    """Read up to ``limit`` messages from one room in chronological order."""

    return _tool_json(lambda: SERVICE.read_messages(room_id=room_id, limit=limit))


@mcp.tool()
def kakaotalk_recent_messages(
    limit: int = DEFAULT_MESSAGE_LIMIT,
    since: str = "",
    until: str = "",
) -> str:
    """Read recent messages, optionally bounded by KST date filters."""

    return _tool_json(
        lambda: SERVICE.recent_messages(limit=limit, since=since, until=until)
    )


@mcp.tool()
def kakaotalk_search_messages(keyword: str, limit: int = DEFAULT_MESSAGE_LIMIT) -> str:
    """Search message text for a literal keyword with a bounded result set."""

    return _tool_json(lambda: SERVICE.search_messages(keyword=keyword, limit=limit))


@mcp.tool()
def kakaotalk_list_users(limit: int = DEFAULT_USER_LIMIT) -> str:
    """List stored KakaoTalk users with bounded, known fields."""

    return _tool_json(lambda: SERVICE.list_users(limit=limit))


@mcp.tool()
def kakaotalk_message_context(
    room_id: int,
    message_id: int,
    before: int = DEFAULT_CONTEXT_RADIUS,
    after: int = DEFAULT_CONTEXT_RADIUS,
) -> str:
    """Return one message plus nearby messages from the same room."""

    return _tool_json(
        lambda: SERVICE.message_context(
            room_id=room_id,
            message_id=message_id,
            before=before,
            after=after,
        )
    )


def main() -> None:
    mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
