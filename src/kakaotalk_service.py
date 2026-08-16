"""Validated domain operations shared by the CLI and the Hermes MCP bridge.

This module deliberately knows about KakaoTalk's small read model, but not
about MCP transport or command-line rendering.  All values that cross the
bridge are validated, selected from known columns, and normalized before
serialization.
"""

from __future__ import annotations

import json
import re
from datetime import datetime, timedelta
from typing import Any, Callable, Mapping

import kakaotalk_reader as reader


KST = reader.KST

DEFAULT_MESSAGE_LIMIT = 50
DEFAULT_ROOM_LIMIT = 50
DEFAULT_USER_LIMIT = 100
DEFAULT_CONTEXT_RADIUS = 3
MAX_MESSAGE_LIMIT = 100
MAX_ROOM_LIMIT = 100
MAX_USER_LIMIT = 200
MAX_CONTEXT_RADIUS = 20
MAX_OUTPUT_BYTES = 600_000
MAX_KEYWORD_CHARS = 256
MAX_TEXT_BYTES = 8_192
MAX_ATTACHMENT_BYTES = 4_096
MAX_ROOM_NAME_BYTES = 512
MAX_DATE_OFFSET_DAYS = 3_650
MAX_SQLITE_INTEGER = 9_223_372_036_854_775_807


class ValidationError(ValueError):
    """An input was outside the deliberately small read-only API contract."""


class ServiceError(RuntimeError):
    """A database operation failed without exposing database contents."""


ROOM_TYPE_LABELS = {
    0: "one-to-one",
    1: "group",
    2: "group",
    3: "open-chat",
    4: "open-chat",
}

MESSAGE_TYPE_LABELS = {
    1: "text",
    2: "photo",
    3: "video",
    4: "audio",
    5: "contact",
    6: "file",
    12: "sticker",
    14: "map",
    16: "schedule",
    18: "voip",
    22: "emoji",
    23: "shout",
    24: "reply",
    26: "gift",
    27: "system",
    51: "liveview",
    71: "voicetalk",
    72: "grouptalk",
}


def validate_id(value: Any, name: str, *, allow_none: bool = False) -> int | None:
    if value is None and allow_none:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValidationError(f"{name} must be a positive integer")
    if value < 1 or value > MAX_SQLITE_INTEGER:
        raise ValidationError(f"{name} must be a positive integer")
    return value


def validate_limit(value: Any, *, default: int, maximum: int) -> int:
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValidationError("limit must be an integer")
    if value < 1 or value > maximum:
        raise ValidationError(f"limit must be between 1 and {maximum}")
    return value


def validate_radius(value: Any, *, default: int = DEFAULT_CONTEXT_RADIUS) -> int:
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValidationError("context radius must be an integer")
    if value < 0 or value > MAX_CONTEXT_RADIUS:
        raise ValidationError(f"context radius must be between 0 and {MAX_CONTEXT_RADIUS}")
    return value


def validate_keyword(value: Any) -> str:
    if not isinstance(value, str):
        raise ValidationError("keyword must be a string")
    if not value or not value.strip():
        raise ValidationError("keyword must not be empty")
    if "\x00" in value:
        raise ValidationError("keyword contains an unsupported character")
    if len(value) > MAX_KEYWORD_CHARS:
        raise ValidationError(f"keyword must be at most {MAX_KEYWORD_CHARS} characters")
    return value


def parse_date(value: Any, *, now: datetime | None = None) -> datetime | None:
    """Parse the CLI-compatible date filters at the start of a KST day."""

    if value is None or value == "":
        return None
    if not isinstance(value, str):
        raise ValidationError("date filters must be strings")

    text = value.strip().lower()
    if not text:
        return None

    current = now or datetime.now(tz=KST)
    if current.tzinfo is None:
        current = current.replace(tzinfo=KST)
    else:
        current = current.astimezone(KST)
    start = current.replace(hour=0, minute=0, second=0, microsecond=0)

    if text == "today":
        return start
    if text == "yesterday":
        return start - timedelta(days=1)

    match = re.fullmatch(r"-(\d+)d", text)
    if match:
        days = int(match.group(1))
        if days > MAX_DATE_OFFSET_DAYS:
            raise ValidationError(f"date offset must be at most {MAX_DATE_OFFSET_DAYS} days")
        return start - timedelta(days=days)

    try:
        return datetime.strptime(text, "%Y-%m-%d").replace(tzinfo=KST)
    except ValueError as exc:
        raise ValidationError("invalid date; use YYYY-MM-DD, today, yesterday, or -Nd") from exc


def _clip_text(value: Any, max_bytes: int) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        text = value.decode("utf-8", errors="replace")
    else:
        text = str(value)
    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return text
    clipped = encoded[: max_bytes - 3].decode("utf-8", errors="ignore")
    return clipped + "..."


def _int_or_none(value: Any) -> int | None:
    if value is None or value == "":
        return None
    try:
        return int(value)
    except (TypeError, ValueError, OverflowError):
        return None


def message_type_label(value: Any) -> str:
    message_type = _int_or_none(value)
    if message_type is None:
        return "unknown"
    return MESSAGE_TYPE_LABELS.get(message_type, f"type:{message_type}")


def room_type_label(value: Any) -> str:
    room_type = _int_or_none(value)
    if room_type is None:
        return "unknown"
    return ROOM_TYPE_LABELS.get(room_type, f"type:{room_type}")


def normalize_message(row: Mapping[str, Any]) -> dict[str, Any]:
    sent_at = _int_or_none(row.get("sentAt"))
    message_type = _int_or_none(row.get("type"))
    chat_id = _int_or_none(row.get("chatId"))
    log_id = _int_or_none(row.get("logId"))
    author_id = _int_or_none(row.get("authorId"))
    room_name = row.get("roomName") or row.get("displayName") or "(unnamed)"
    author_name = row.get("nickName") or row.get("authorName") or ""

    normalized: dict[str, Any] = {
        "chatId": chat_id,
        "roomName": _clip_text(room_name, MAX_ROOM_NAME_BYTES),
        "logId": log_id,
        "authorId": author_id,
        "nickName": _clip_text(author_name, MAX_ROOM_NAME_BYTES),
        "type": message_type,
        "typeLabel": message_type_label(message_type),
        "sentAt": sent_at,
        "time": reader.fmt_ts(sent_at),
        "message": _clip_text(row.get("message"), MAX_TEXT_BYTES),
    }
    attachment = _clip_text(row.get("attachment"), MAX_ATTACHMENT_BYTES)
    if attachment:
        normalized["attachment"] = attachment
    return normalized


def normalize_room(row: Mapping[str, Any]) -> dict[str, Any]:
    room_type = _int_or_none(row.get("type"))
    display_name = row.get("displayName") or row.get("chatName") or "(unnamed)"
    return {
        "chatId": _int_or_none(row.get("chatId")),
        "displayName": _clip_text(display_name, MAX_ROOM_NAME_BYTES),
        "roomType": room_type_label(room_type),
        "type": room_type,
        "chatName": _clip_text(row.get("chatName"), MAX_ROOM_NAME_BYTES),
        "linkId": _int_or_none(row.get("linkId")),
        "lastLogId": _int_or_none(row.get("lastLogId")),
        "directChatMemberUserId": _int_or_none(row.get("directChatMemberUserId")),
        "activeMembersCount": _int_or_none(row.get("activeMembersCount")),
    }


def normalize_user(row: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "userId": _int_or_none(row.get("userId")),
        "linkId": _int_or_none(row.get("linkId")),
        "nickName": _clip_text(row.get("nickName"), MAX_ROOM_NAME_BYTES),
    }


def serialize_json(value: Any) -> str:
    """Serialize stable JSON and cap oversized list responses deterministically."""

    def encode(item: Any) -> str:
        return json.dumps(item, ensure_ascii=False, sort_keys=True, separators=(",", ":"))

    encoded = encode(value)
    if len(encoded.encode("utf-8")) <= MAX_OUTPUT_BYTES:
        return encoded

    if isinstance(value, list):
        for size in range(len(value) - 1, 0, -1):
            encoded = encode(value[:size])
            if len(encoded.encode("utf-8")) <= MAX_OUTPUT_BYTES:
                return encoded
    raise ServiceError("result exceeds the output limit; reduce the requested limit")


def _escape_like(value: str) -> str:
    return value.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


MESSAGE_SELECT = """
SELECT m.chatId, m.logId, m.authorId, m.type, m.sentAt, m.message, m.attachment,
       u.nickName, COALESCE(NULLIF(r.chatName, ''), du.nickName) AS roomName,
       r.directChatMemberUserId
FROM NTChatMessage m
LEFT JOIN NTChatRoom r ON m.chatId = r.chatId
LEFT JOIN NTUser u ON m.authorId = u.userId AND u.linkId = COALESCE(r.linkId, 0)
LEFT JOIN NTUser du ON r.directChatMemberUserId = du.userId
                   AND du.linkId = COALESCE(r.linkId, 0)
"""


class _ResolveArgs:
    uuid = None
    user_id = None
    db_path = None
    format = "json"


class KakaoTalkService:
    """Read-only operations with injectable resolver/query functions for tests."""

    def __init__(
        self,
        *,
        resolver: Callable[[], tuple[str, str]] | None = None,
        query: Callable[..., list[dict[str, Any]]] | None = None,
    ) -> None:
        self._resolver = resolver or (lambda: reader.resolve(_ResolveArgs()))
        self._query = query or reader.query_json

    def _connection(self) -> tuple[str, str]:
        try:
            resolved = self._resolver()
            if (
                not isinstance(resolved, tuple)
                or len(resolved) != 2
                or not isinstance(resolved[0], str)
                or not isinstance(resolved[1], str)
            ):
                raise TypeError("resolver returned an invalid connection")
            return resolved
        except ValidationError:
            raise
        except Exception as exc:
            raise ServiceError("KakaoTalk setup could not be resolved") from exc

    def _run_query(
        self,
        path: str,
        key: str,
        sql: str,
        *,
        params: Mapping[str, Any] | None = None,
        max_rows: int | None = None,
    ) -> list[dict[str, Any]]:
        try:
            return self._query(
                path,
                key,
                sql,
                params=params,
                read_only=True,
                max_rows=max_rows,
            )
        except ValidationError:
            raise
        except Exception as exc:
            raise ServiceError(
                "KakaoTalk database read failed; check setup and that KakaoTalk is available"
            ) from exc

    def _fallback_room_name(self, path: str, key: str, row: Mapping[str, Any]) -> str:
        chat_id = _int_or_none(row.get("chatId"))
        link_id = _int_or_none(row.get("linkId")) or 0
        if chat_id is None:
            return "(unnamed)"
        try:
            members = self._run_query(
                path,
                key,
                """
                SELECT u.nickName
                FROM (
                    SELECT DISTINCT authorId
                    FROM NTChatMessage
                    WHERE chatId = :chat_id
                    LIMIT :member_limit
                ) a
                JOIN NTUser u ON a.authorId = u.userId AND u.linkId = :link_id
                ORDER BY u.nickName COLLATE NOCASE
                LIMIT :name_limit;
                """,
                params={
                    "chat_id": chat_id,
                    "member_limit": 20,
                    "link_id": link_id,
                    "name_limit": 4,
                },
                max_rows=4,
            )
        except ServiceError:
            return "(unnamed)"
        names = [_clip_text(member.get("nickName"), MAX_ROOM_NAME_BYTES) for member in members]
        names = [name for name in names if name]
        return ", ".join(names) if names else "(unnamed)"

    def list_rooms(self, *, limit: Any = DEFAULT_ROOM_LIMIT) -> list[dict[str, Any]]:
        bounded = validate_limit(limit, default=DEFAULT_ROOM_LIMIT, maximum=MAX_ROOM_LIMIT)
        path, key = self._connection()
        rows = self._run_query(
            path,
            key,
            """
            SELECT r.chatId, r.type, r.chatName, r.linkId, r.lastLogId,
                   r.directChatMemberUserId, r.activeMembersCount,
                   COALESCE(NULLIF(r.chatName, ''), du.nickName) AS displayName
            FROM NTChatRoom r
            LEFT JOIN NTUser du ON r.directChatMemberUserId = du.userId
                               AND du.linkId = COALESCE(r.linkId, 0)
            ORDER BY r.lastLogId DESC, r.chatId ASC
            LIMIT :limit;
            """,
            params={"limit": bounded},
            max_rows=bounded,
        )
        normalized = []
        for row in rows:
            if not row.get("displayName") and not row.get("chatName"):
                row["displayName"] = self._fallback_room_name(path, key, row)
            normalized.append(normalize_room(row))
        return normalized

    def read_messages(self, *, room_id: Any, limit: Any = DEFAULT_MESSAGE_LIMIT) -> list[dict[str, Any]]:
        validated_room_id = validate_id(room_id, "room_id", allow_none=True)
        bounded = validate_limit(limit, default=DEFAULT_MESSAGE_LIMIT, maximum=MAX_MESSAGE_LIMIT)
        path, key = self._connection()
        conditions: list[str] = []
        params: dict[str, Any] = {"limit": bounded}
        if validated_room_id is not None:
            conditions.append("m.chatId = :room_id")
            params["room_id"] = validated_room_id
        where = "WHERE " + " AND ".join(conditions) if conditions else ""
        rows = self._run_query(
            path,
            key,
            f"""
            {MESSAGE_SELECT}
            {where}
            ORDER BY m.logId DESC, m.chatId ASC
            LIMIT :limit;
            """,
            params=params,
            max_rows=bounded,
        )
        rows.reverse()
        return [normalize_message(row) for row in rows]

    def recent_messages(
        self,
        *,
        limit: Any = DEFAULT_MESSAGE_LIMIT,
        since: Any = "",
        until: Any = "",
    ) -> list[dict[str, Any]]:
        bounded = validate_limit(limit, default=DEFAULT_MESSAGE_LIMIT, maximum=MAX_MESSAGE_LIMIT)
        since_dt = parse_date(since)
        until_dt = parse_date(until)
        if since_dt and until_dt and until_dt < since_dt:
            raise ValidationError("until must be on or after since")

        conditions: list[str] = []
        params: dict[str, Any] = {"limit": bounded}
        if since_dt:
            conditions.append("m.sentAt >= :since_ts")
            params["since_ts"] = int(since_dt.timestamp())
        if until_dt:
            conditions.append("m.sentAt < :until_ts")
            params["until_ts"] = int(until_dt.timestamp()) + 86_400
        where = "WHERE " + " AND ".join(conditions) if conditions else ""

        path, key = self._connection()
        rows = self._run_query(
            path,
            key,
            f"""
            {MESSAGE_SELECT}
            {where}
            ORDER BY m.sentAt DESC, m.logId DESC, m.chatId ASC
            LIMIT :limit;
            """,
            params=params,
            max_rows=bounded,
        )
        rows.reverse()
        return [normalize_message(row) for row in rows]

    def search_messages(self, *, keyword: Any, limit: Any = DEFAULT_MESSAGE_LIMIT) -> list[dict[str, Any]]:
        search_term = _escape_like(validate_keyword(keyword))
        bounded = validate_limit(limit, default=DEFAULT_MESSAGE_LIMIT, maximum=MAX_MESSAGE_LIMIT)
        path, key = self._connection()
        rows = self._run_query(
            path,
            key,
            f"""
            {MESSAGE_SELECT}
            WHERE m.message LIKE '%' || CAST(:keyword AS TEXT) || '%' ESCAPE '\\'
            ORDER BY m.sentAt DESC, m.logId DESC, m.chatId ASC
            LIMIT :limit;
            """,
            params={"keyword": search_term, "limit": bounded},
            max_rows=bounded,
        )
        rows.reverse()
        return [normalize_message(row) for row in rows]

    def list_users(self, *, limit: Any = DEFAULT_USER_LIMIT) -> list[dict[str, Any]]:
        bounded = validate_limit(limit, default=DEFAULT_USER_LIMIT, maximum=MAX_USER_LIMIT)
        path, key = self._connection()
        rows = self._run_query(
            path,
            key,
            """
            SELECT userId, linkId, nickName
            FROM NTUser
            ORDER BY COALESCE(nickName, ''), userId ASC, linkId ASC
            LIMIT :limit;
            """,
            params={"limit": bounded},
            max_rows=bounded,
        )
        return [normalize_user(row) for row in rows]

    def message_context(
        self,
        *,
        room_id: Any,
        message_id: Any,
        before: Any = DEFAULT_CONTEXT_RADIUS,
        after: Any = DEFAULT_CONTEXT_RADIUS,
    ) -> dict[str, Any]:
        validated_room_id = validate_id(room_id, "room_id")
        validated_message_id = validate_id(message_id, "message_id")
        before_count = validate_radius(before)
        after_count = validate_radius(after)
        path, key = self._connection()

        target_rows = self._run_query(
            path,
            key,
            f"""
            {MESSAGE_SELECT}
            WHERE m.chatId = :room_id AND m.logId = :message_id
            LIMIT :one;
            """,
            params={"room_id": validated_room_id, "message_id": validated_message_id, "one": 1},
            max_rows=1,
        )
        if not target_rows:
            raise ValidationError("message_id was not found in room_id")

        before_rows: list[dict[str, Any]] = []
        if before_count:
            before_rows = self._run_query(
                path,
                key,
                f"""
                {MESSAGE_SELECT}
                WHERE m.chatId = :room_id AND m.logId < :message_id
                ORDER BY m.logId DESC, m.chatId ASC
                LIMIT :limit;
                """,
                params={
                    "room_id": validated_room_id,
                    "message_id": validated_message_id,
                    "limit": before_count,
                },
                max_rows=before_count,
            )
            before_rows.reverse()

        after_rows: list[dict[str, Any]] = []
        if after_count:
            after_rows = self._run_query(
                path,
                key,
                f"""
                {MESSAGE_SELECT}
                WHERE m.chatId = :room_id AND m.logId > :message_id
                ORDER BY m.logId ASC, m.chatId ASC
                LIMIT :limit;
                """,
                params={
                    "room_id": validated_room_id,
                    "message_id": validated_message_id,
                    "limit": after_count,
                },
                max_rows=after_count,
            )

        target = normalize_message(target_rows[0])
        return {
            "chatId": validated_room_id,
            "messageId": validated_message_id,
            "before": [normalize_message(row) for row in before_rows],
            "message": target,
            "after": [normalize_message(row) for row in after_rows],
        }
