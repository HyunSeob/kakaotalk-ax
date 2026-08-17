"""Validated domain operations shared by the CLI and the Hermes MCP bridge.

This module deliberately knows about KakaoTalk's small read model, but not
about MCP transport or command-line rendering.  All values that cross the
bridge are validated, selected from known columns, and normalized before
serialization.
"""

from __future__ import annotations

import base64
import json
import math
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
MAX_OPTIONAL_METADATA_BYTES = 4_096
MAX_OPTIONAL_METADATA_DEPTH = 4
MAX_OPTIONAL_METADATA_ITEMS = 32
MAX_OPTIONAL_METADATA_KEY_BYTES = 128
MAX_ROOM_NAME_BYTES = 512
MAX_DATE_OFFSET_DAYS = 3_650
MAX_SQLITE_INTEGER = 9_223_372_036_854_775_807
DEFAULT_UPDATE_LIMIT = 50
MAX_UPDATE_LIMIT = 100
MAX_GLOBAL_ROOMS = 384
MAX_CURSOR_CHARS = 256
MAX_CURSOR_JSON_BYTES = 192
CURSOR_PREFIX = "kt2."
CURSOR_VERSION = 1

MESSAGE_REQUIRED_COLUMNS = frozenset(
    {"chatid", "logid", "authorid", "type", "sentat", "message"}
)
MESSAGE_OPTIONAL_COLUMNS = (
    "attachment",
    "supplement",
    "extra",
    "thread",
    "scope",
    "systemMessage",
)


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


def _redact_local_path(text: str) -> str:
    text = re.sub(r"file://[^\s\"'<>]+", "[local path omitted]", text)
    return re.sub(r"(?<![A-Za-z0-9_/:])/(?!/)[^\s\"'<>]*", "[local path omitted]", text)


def _clip_text(value: Any, max_bytes: int, *, redact_paths: bool = False) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        text = value.decode("utf-8", errors="replace")
    else:
        text = str(value)
    if redact_paths:
        text = _redact_local_path(text)
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


def _bounded_metadata(value: Any, *, depth: int = 0, budget: list[int] | None = None) -> Any:
    if budget is None:
        budget = [MAX_OPTIONAL_METADATA_BYTES]
    if budget[0] <= 0 or depth > MAX_OPTIONAL_METADATA_DEPTH:
        return "[truncated]"
    if value is None or isinstance(value, bool):
        budget[0] -= 4
        return value
    if isinstance(value, int):
        if abs(value) <= MAX_SQLITE_INTEGER:
            budget[0] -= min(budget[0], len(str(value).encode("ascii")))
            return value
        value = _clip_text(value, MAX_TEXT_BYTES, redact_paths=True)
        budget[0] -= len(value.encode("utf-8"))
        return value
    if isinstance(value, float):
        if math.isfinite(value):
            budget[0] -= min(budget[0], len(str(value).encode("ascii")))
            return value
        budget[0] -= len("[non-finite]")
        return "[non-finite]"
    if isinstance(value, str) or isinstance(value, bytes):
        limit = min(MAX_OPTIONAL_METADATA_BYTES, budget[0])
        if limit < 4:
            budget[0] = 0
            return "[truncated]"
        clipped = _clip_text(value, limit, redact_paths=True)
        budget[0] -= len(clipped.encode("utf-8"))
        return clipped
    if isinstance(value, Mapping):
        bounded: dict[str, Any] = {}
        for raw_key in sorted(value, key=lambda item: str(item))[:MAX_OPTIONAL_METADATA_ITEMS]:
            if budget[0] <= 0:
                break
            key_limit = min(MAX_OPTIONAL_METADATA_KEY_BYTES, budget[0])
            if key_limit < 4:
                break
            key = _clip_text(raw_key, key_limit, redact_paths=True)
            budget[0] -= len(key.encode("utf-8")) + 2
            bounded[key] = _bounded_metadata(value[raw_key], depth=depth + 1, budget=budget)
        return bounded
    if isinstance(value, (list, tuple)):
        bounded_list = []
        for item in list(value)[:MAX_OPTIONAL_METADATA_ITEMS]:
            if budget[0] <= 0:
                break
            bounded_list.append(_bounded_metadata(item, depth=depth + 1, budget=budget))
        return bounded_list
    if budget[0] < 4:
        budget[0] = 0
        return "[truncated]"
    fallback = _clip_text(value, min(MAX_OPTIONAL_METADATA_BYTES, budget[0]), redact_paths=True)
    budget[0] -= len(fallback.encode("utf-8"))
    return fallback


def _normalize_structured_value(value: Any) -> dict[str, Any]:
    truncated = False
    if isinstance(value, (Mapping, list, tuple)):
        parsed = value
        parsed_json = True
    else:
        raw = _clip_text(value, MAX_OPTIONAL_METADATA_BYTES, redact_paths=True)
        original_bytes = len(value.encode("utf-8")) if isinstance(value, str) else len(str(value).encode("utf-8"))
        truncated = original_bytes > MAX_OPTIONAL_METADATA_BYTES
        try:
            parsed = json.loads(raw)
            parsed_json = True
        except (TypeError, json.JSONDecodeError):
            return {"kind": "raw", "truncated": truncated, "value": raw}
    if not parsed_json:
        return {"kind": "raw", "truncated": truncated, "value": _clip_text(parsed, MAX_OPTIONAL_METADATA_BYTES, redact_paths=True)}
    return {"kind": "json", "truncated": truncated, "value": _bounded_metadata(parsed)}


def normalize_message(row: Mapping[str, Any], *, extended: bool = True) -> dict[str, Any]:
    sent_at = _int_or_none(row.get("sentAt"))
    message_type = _int_or_none(row.get("type"))
    chat_id = _int_or_none(row.get("chatId"))
    log_id = _int_or_none(row.get("logId"))
    author_id = _int_or_none(row.get("authorId"))
    room_name = row.get("roomName") or row.get("displayName") or "(unnamed)"
    author_name = row.get("nickName") or row.get("authorName") or ""

    normalized: dict[str, Any] = {
        "chatId": chat_id,
        "roomName": _clip_text(room_name, MAX_ROOM_NAME_BYTES, redact_paths=True),
        "logId": log_id,
        "authorId": author_id,
        "nickName": _clip_text(author_name, MAX_ROOM_NAME_BYTES, redact_paths=True),
        "type": message_type,
        "typeLabel": message_type_label(message_type),
        "sentAt": sent_at,
        "time": reader.fmt_ts(sent_at),
        "message": _clip_text(row.get("message"), MAX_TEXT_BYTES, redact_paths=True),
    }
    attachment = _clip_text(row.get("attachment"), MAX_ATTACHMENT_BYTES, redact_paths=True)
    if attachment:
        normalized["attachment"] = attachment
    if not extended:
        return normalized

    if row.get("msgId") is not None:
        normalized["msgId"] = _int_or_none(row.get("msgId"))
    if "revision" in row:
        normalized["revision"] = _int_or_none(row.get("revision")) or 0
    if attachment:
        normalized["attachmentInfo"] = _normalize_structured_value(row.get("attachment"))
    for field in ("supplement", "extra", "thread", "scope"):
        if row.get(field) not in (None, ""):
            normalized[field] = _normalize_structured_value(row.get(field))

    system_message = row.get("systemMessage")
    if message_type == 27 or system_message not in (None, ""):
        normalized["system"] = {"isSystem": message_type == 27}
        if system_message not in (None, ""):
            normalized["system"]["metadata"] = _normalize_structured_value(system_message)
    return normalized


def normalize_room(row: Mapping[str, Any]) -> dict[str, Any]:
    room_type = _int_or_none(row.get("type"))
    display_name = row.get("displayName") or row.get("chatName") or "(unnamed)"
    return {
        "chatId": _int_or_none(row.get("chatId")),
        "displayName": _clip_text(display_name, MAX_ROOM_NAME_BYTES, redact_paths=True),
        "roomType": room_type_label(room_type),
        "type": room_type,
        "chatName": _clip_text(row.get("chatName"), MAX_ROOM_NAME_BYTES, redact_paths=True),
        "linkId": _int_or_none(row.get("linkId")),
        "lastLogId": _int_or_none(row.get("lastLogId")),
        "directChatMemberUserId": _int_or_none(row.get("directChatMemberUserId")),
        "activeMembersCount": _int_or_none(row.get("activeMembersCount")),
    }


def normalize_user(row: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "userId": _int_or_none(row.get("userId")),
        "linkId": _int_or_none(row.get("linkId")),
        "nickName": _clip_text(row.get("nickName"), MAX_ROOM_NAME_BYTES, redact_paths=True),
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


def _validate_cursor_integer(value: Any, name: str, *, minimum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValidationError(f"cursor {name} must be an integer")
    if value < minimum or value > MAX_SQLITE_INTEGER:
        raise ValidationError(f"cursor {name} is outside the supported range")
    return value


def encode_cursor(ordering_key: tuple[int, int, int, int], *, scope: int | None = None) -> str:
    """Encode a versioned cursor containing IDs only, never message data."""

    if not isinstance(ordering_key, tuple) or len(ordering_key) != 4:
        raise ValidationError("cursor ordering key must contain four integers")
    chat_id = _validate_cursor_integer(ordering_key[0], "chatId", minimum=1)
    log_id = _validate_cursor_integer(ordering_key[1], "logId", minimum=1)
    msg_id = _validate_cursor_integer(ordering_key[2], "msgId", minimum=0)
    revision = _validate_cursor_integer(ordering_key[3], "revision", minimum=0)
    if scope is not None:
        scope = validate_id(scope, "room_id")

    payload = {"k": [chat_id, log_id, msg_id, revision], "scope": scope, "v": CURSOR_VERSION}
    raw = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if len(raw) > MAX_CURSOR_JSON_BYTES:
        raise ValidationError("cursor is too large")
    encoded = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
    result = CURSOR_PREFIX + encoded
    if len(result) > MAX_CURSOR_CHARS:
        raise ValidationError("cursor is too large")
    return result


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def decode_cursor(value: Any) -> tuple[tuple[int, int, int, int], int | None] | None:
    """Decode the current cursor format; an empty value means the initial page."""

    if value is None or value == "":
        return None
    if not isinstance(value, str):
        raise ValidationError("cursor must be a string")
    if len(value) > MAX_CURSOR_CHARS or not value.startswith(CURSOR_PREFIX):
        raise ValidationError("cursor is invalid")

    encoded = value[len(CURSOR_PREFIX):]
    if not encoded or not re.fullmatch(r"[A-Za-z0-9_-]+={0,2}", encoded):
        raise ValidationError("cursor is invalid")
    unpadded = encoded.rstrip("=")
    if "=" in unpadded:
        raise ValidationError("cursor is invalid")
    try:
        raw = base64.urlsafe_b64decode(unpadded + "=" * (-len(unpadded) % 4))
        if len(raw) > MAX_CURSOR_JSON_BYTES:
            raise ValueError("cursor payload is too large")
        canonical = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
        if canonical != unpadded:
            raise ValueError("cursor encoding is not canonical")
        payload = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=lambda _: (_ for _ in ()).throw(ValueError("invalid JSON number")),
        )
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValidationError("cursor is invalid") from exc

    if not isinstance(payload, dict) or set(payload) != {"k", "scope", "v"}:
        raise ValidationError("cursor is invalid")
    if (
        isinstance(payload["v"], bool)
        or not isinstance(payload["v"], int)
        or payload["v"] != CURSOR_VERSION
    ):
        raise ValidationError("cursor version is unsupported")
    key = payload["k"]
    if not isinstance(key, list) or len(key) != 4:
        raise ValidationError("cursor ordering key is invalid")
    ordering_key = (
        _validate_cursor_integer(key[0], "chatId", minimum=1),
        _validate_cursor_integer(key[1], "logId", minimum=1),
        _validate_cursor_integer(key[2], "msgId", minimum=0),
        _validate_cursor_integer(key[3], "revision", minimum=0),
    )
    scope = payload["scope"]
    if scope is not None:
        scope = validate_id(scope, "room_id")
    return ordering_key, scope


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
        status_reader: Callable[[str], Mapping[str, Any]] | None = None,
    ) -> None:
        self._resolver = resolver or (lambda: reader.resolve(_ResolveArgs()))
        self._query = query or reader.query_json
        self._status_reader = status_reader or reader.database_file_status
        self._last_status_signature: tuple[Any, Any, Any] | None = None

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

    def status(self) -> dict[str, Any]:
        """Return a bounded process-local database/WAL change signal."""

        try:
            path, _ = self._connection()
            observation = self._status_reader(path)
            available = bool(observation.get("available"))
            wal_present = bool(observation.get("wal_present"))
            database_signature = observation.get("database_signature")
            wal_signature = observation.get("wal_signature")
        except Exception:
            available = False
            wal_present = False
            database_signature = None
            wal_signature = None

        previous = self._last_status_signature
        self._last_status_signature = (available, database_signature, wal_signature)
        change_known = previous is not None
        return {
            "databaseAvailable": available,
            "walPresent": wal_present,
            "walChanged": bool(change_known and wal_signature != previous[2]),
            "databaseChanged": bool(change_known and database_signature != previous[1]),
            "changeKnown": change_known,
        }

    def source_signature(self) -> dict[str, Any]:
        """Return only the current database/WAL signatures for a local poller."""

        try:
            path, _ = self._connection()
            observation = self._status_reader(path)
            if not observation.get("available"):
                raise ServiceError("KakaoTalk database is unavailable")
            return {
                "database": observation.get("database_signature"),
                "wal": observation.get("wal_signature"),
            }
        except ValidationError:
            raise
        except ServiceError:
            raise
        except Exception as exc:
            raise ServiceError("KakaoTalk source status is unavailable") from exc

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

    def _table_columns(self, path: str, key: str, table: str) -> set[str]:
        if table not in {"NTChatMessage", "NTChatRoom", "NTUser"}:
            raise ValueError("unsupported schema table")
        rows = self._run_query(
            path,
            key,
            f"PRAGMA table_info({table});",
            max_rows=300,
        )
        return {
            str(row.get("name")).lower()
            for row in rows
            if row.get("name") is not None and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", str(row.get("name")))
        }

    def _updates_projection(
        self,
        path: str,
        key: str,
    ) -> tuple[list[str], str, str, str, bool]:
        message_columns = self._table_columns(path, key, "NTChatMessage")
        missing = MESSAGE_REQUIRED_COLUMNS - message_columns
        if missing:
            raise ServiceError("KakaoTalk message schema is unavailable")

        try:
            room_columns = self._table_columns(path, key, "NTChatRoom")
        except ServiceError:
            room_columns = set()
        try:
            user_columns = self._table_columns(path, key, "NTUser")
        except ServiceError:
            user_columns = set()

        def message_column(column: str) -> str:
            if column.lower() in message_columns:
                return f'm."{column}" AS "{column}"'
            return f'NULL AS "{column}"'

        msg_id_expr = (
            'COALESCE(m."msgId", m."logId")'
            if "msgid" in message_columns
            else 'm."logId"'
        )
        revision_expr = (
            'COALESCE(m."revision", 0)'
            if "revision" in message_columns
            else "0"
        )
        select_parts = [
            'm."chatId" AS "chatId"',
            'm."logId" AS "logId"',
            'm."authorId" AS "authorId"',
            'm."type" AS "type"',
            'm."sentAt" AS "sentAt"',
            'm."message" AS "message"',
            f"{msg_id_expr} AS \"msgId\"",
            f"{revision_expr} AS \"revision\"",
        ]
        select_parts.extend(message_column(column) for column in MESSAGE_OPTIONAL_COLUMNS)

        joins: list[str] = []
        room_name = "NULL AS \"roomName\""
        has_last_log_id = False
        if "chatid" in room_columns:
            joins.append('LEFT JOIN NTChatRoom r ON m."chatId" = r."chatId"')
            has_last_log_id = "lastlogid" in room_columns
            chat_name = 'NULLIF(r."chatName", \'\')' if "chatname" in room_columns else "NULL"
            if {
                "userid",
                "nickname",
            }.issubset(user_columns):
                author_join = 'u."userId" = m."authorId"'
                if "linkid" in room_columns and "linkid" in user_columns:
                    author_join += ' AND u."linkId" = COALESCE(r."linkId", 0)'
                joins.append(f'LEFT JOIN NTUser u ON {author_join}')
                author_name = 'u."nickName"'
            else:
                author_name = "NULL"

            if {
                "directchatmemberuserid",
                "linkid",
            }.issubset(room_columns) and {"userid", "nickname"}.issubset(user_columns):
                direct_join = 'du."userId" = r."directChatMemberUserId"'
                if "linkid" in user_columns:
                    direct_join += ' AND du."linkId" = COALESCE(r."linkId", 0)'
                joins.append(f'LEFT JOIN NTUser du ON {direct_join}')
                room_name = f'COALESCE({chat_name}, du."nickName") AS "roomName"'
            else:
                room_name = f'{chat_name} AS "roomName"'
            select_parts.extend([f'{author_name} AS "nickName"', room_name])
        elif {"userid", "nickname"}.issubset(user_columns):
            joins.append('LEFT JOIN NTUser u ON u."userId" = m."authorId"')
            select_parts.extend(['u."nickName" AS "nickName"', 'NULL AS "roomName"'])
        else:
            select_parts.extend(['NULL AS "nickName"', 'NULL AS "roomName"'])

        return select_parts, msg_id_expr, revision_expr, "\n".join(joins), has_last_log_id

    @staticmethod
    def _ordering_key(row: Mapping[str, Any]) -> tuple[int, int, int, int]:
        chat_id = _int_or_none(row.get("chatId"))
        log_id = _int_or_none(row.get("logId"))
        msg_id = _int_or_none(row.get("msgId"))
        revision = _int_or_none(row.get("revision"))
        if msg_id is None:
            msg_id = log_id
        if revision is None:
            revision = 0
        if (
            chat_id is None
            or log_id is None
            or chat_id < 1
            or log_id < 1
            or msg_id is None
            or msg_id < 0
            or revision < 0
        ):
            raise ServiceError("KakaoTalk database returned an invalid message ordering key")
        return chat_id, log_id, msg_id, revision

    @staticmethod
    def _cursor_condition(msg_id_expr: str, revision_expr: str) -> str:
        return f"""(
            m."chatId" > :cursor_chat_id
            OR (m."chatId" = :cursor_chat_id AND m."logId" > :cursor_log_id)
            OR (m."chatId" = :cursor_chat_id AND m."logId" = :cursor_log_id
                AND {msg_id_expr} > :cursor_msg_id)
            OR (m."chatId" = :cursor_chat_id AND m."logId" = :cursor_log_id
                AND {msg_id_expr} = :cursor_msg_id
                AND {revision_expr} > :cursor_revision)
        )"""

    def updates(
        self,
        *,
        cursor: Any = "",
        limit: Any = DEFAULT_UPDATE_LIMIT,
        room_id: Any = None,
    ) -> dict[str, Any]:
        bounded = validate_limit(limit, default=DEFAULT_UPDATE_LIMIT, maximum=MAX_UPDATE_LIMIT)
        validated_room_id = validate_id(room_id, "room_id", allow_none=True)
        decoded_cursor = decode_cursor(cursor)
        if decoded_cursor and decoded_cursor[1] != validated_room_id:
            raise ValidationError("cursor room scope does not match room_id")

        path, key = self._connection()
        select_parts, msg_id_expr, revision_expr, joins, has_last_log_id = self._updates_projection(path, key)
        conditions: list[str] = []
        params: dict[str, Any] = {"fetch_limit": bounded + 1}
        if validated_room_id is not None:
            conditions.append('m."chatId" = :room_id')
            params["room_id"] = validated_room_id
        if decoded_cursor:
            ordering_key, _ = decoded_cursor
            params.update(
                {
                    "cursor_chat_id": ordering_key[0],
                    "cursor_log_id": ordering_key[1],
                    "cursor_msg_id": ordering_key[2],
                    "cursor_revision": ordering_key[3],
                }
            )
            conditions.append(self._cursor_condition(msg_id_expr, revision_expr))
        if has_last_log_id:
            conditions.append('(r."lastLogId" IS NULL OR m."logId" <= r."lastLogId")')
        where = "WHERE " + " AND ".join(conditions) if conditions else ""
        select_sql = ", ".join(select_parts)
        rows = self._run_query(
            path,
            key,
            f"""
            SELECT {select_sql}
            FROM NTChatMessage m
            {joins}
            {where}
            ORDER BY m."chatId" ASC, m."logId" ASC, {msg_id_expr} ASC, {revision_expr} ASC
            LIMIT :fetch_limit;
            """,
            params=params,
            max_rows=bounded + 1,
        )
        rows.sort(key=self._ordering_key)
        has_more = len(rows) > bounded
        page_rows = rows[:bounded]
        messages = []
        for row in page_rows:
            ordering_key = self._ordering_key(row)
            normalized_row = dict(row)
            if normalized_row.get("msgId") is None:
                normalized_row["msgId"] = ordering_key[2]
            if normalized_row.get("revision") is None:
                normalized_row["revision"] = ordering_key[3]
            messages.append(normalize_message(normalized_row))
        next_cursor = cursor if decoded_cursor else None
        if page_rows:
            next_cursor = encode_cursor(self._ordering_key(page_rows[-1]), scope=validated_room_id)

        while messages:
            envelope = {"hasMore": has_more, "messages": messages, "nextCursor": next_cursor}
            if len(json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")) <= MAX_OUTPUT_BYTES:
                return envelope
            messages.pop()
            has_more = True
            last_index = len(messages) - 1
            next_cursor = (
                encode_cursor(self._ordering_key(page_rows[last_index]), scope=validated_room_id)
                if messages
                else cursor if decoded_cursor else None
            )
        if page_rows:
            raise ServiceError("result exceeds the output limit; reduce the requested limit")
        return {"hasMore": False, "messages": [], "nextCursor": next_cursor}

    def room_ids(self, *, limit: Any = MAX_ROOM_LIMIT) -> list[int]:
        """Return a bounded, deterministic set of usable room IDs."""

        bounded = validate_limit(limit, default=MAX_ROOM_LIMIT, maximum=MAX_GLOBAL_ROOMS)
        path, key = self._connection()
        message_columns = self._table_columns(path, key, "NTChatMessage")
        if "chatid" not in message_columns:
            raise ServiceError("KakaoTalk message schema is unavailable")

        try:
            room_columns = self._table_columns(path, key, "NTChatRoom")
        except ServiceError:
            room_columns = set()

        def usable_chat_id(alias: str) -> str:
            column = f'{alias}."chatId"'
            return f"""(
                typeof({column}) = 'integer'
                OR (
                    typeof({column}) = 'text'
                    AND trim({column}) <> ''
                    AND trim({column}) NOT GLOB '*[^0-9]*'
                )
            ) AND CAST({column} AS INTEGER) > 0"""

        sources = [
            f'''SELECT CAST(m."chatId" AS INTEGER) AS "chatId"
                FROM NTChatMessage m
                WHERE {usable_chat_id("m")}'''
        ]
        if "chatid" in room_columns:
            sources.append(
                f'''SELECT CAST(r."chatId" AS INTEGER) AS "chatId"
                    FROM NTChatRoom r
                    WHERE {usable_chat_id("r")}'''
            )

        rows = self._run_query(
            path,
            key,
            f'''
            SELECT DISTINCT discovered."chatId" AS "chatId"
            FROM ({" UNION ALL ".join(sources)}) AS discovered
            WHERE discovered."chatId" > 0
            ORDER BY discovered."chatId" ASC
            LIMIT :fetch_limit;
            ''',
            params={"fetch_limit": bounded + 1},
            max_rows=bounded + 1,
        )
        room_ids: list[int] = []
        for row in rows:
            room_id = _int_or_none(row.get("chatId"))
            if room_id is None or room_id < 1:
                # The SQL predicate should exclude these rows.  Keep the
                # boundary fail-closed if a driver returns an unexpected row.
                raise ServiceError("KakaoTalk database returned an invalid room ID")
            room_ids.append(room_id)

        room_ids = sorted(set(room_ids))
        if len(room_ids) > bounded:
            raise ServiceError("too many KakaoTalk rooms for one global poll")
        return room_ids

    @staticmethod
    def _validated_global_room_ids(room_ids: Any) -> list[int]:
        if not isinstance(room_ids, (list, tuple, set, frozenset)):
            raise ValidationError("global room IDs must be a bounded collection")
        if len(room_ids) > MAX_GLOBAL_ROOMS:
            raise ValidationError("too many KakaoTalk rooms for one global poll")
        normalized: set[int] = set()
        for raw_room_id in room_ids:
            normalized_id = validate_id(raw_room_id, "room_id")
            if normalized_id is None:
                raise ValidationError("room_id must be a positive integer")
            normalized.add(normalized_id)
        return sorted(normalized)

    def high_water_cursors(self, *, room_ids: Any) -> dict[int, str | None]:
        """Return one numeric high-water cursor per bounded room in one read."""

        normalized_room_ids = self._validated_global_room_ids(room_ids)
        if not normalized_room_ids:
            return {}

        path, key = self._connection()
        message_columns = self._table_columns(path, key, "NTChatMessage")
        if not {"chatid", "logid"}.issubset(message_columns):
            raise ServiceError("KakaoTalk message schema is unavailable")
        try:
            room_columns = self._table_columns(path, key, "NTChatRoom")
        except ServiceError:
            room_columns = set()

        msg_id_expr = (
            'COALESCE(m."msgId", m."logId")'
            if "msgid" in message_columns
            else 'm."logId"'
        )
        revision_expr = (
            'COALESCE(m."revision", 0)'
            if "revision" in message_columns
            else "0"
        )
        joins = ""
        has_last_log_id = False
        if "chatid" in room_columns:
            joins = 'LEFT JOIN NTChatRoom r ON m."chatId" = r."chatId"'
            has_last_log_id = "lastlogid" in room_columns

        room_params = {
            f"room_{index}": room_id
            for index, room_id in enumerate(normalized_room_ids)
        }
        placeholders = ", ".join(f":room_{index}" for index in range(len(normalized_room_ids)))
        conditions = [f'm."chatId" IN ({placeholders})']
        if has_last_log_id:
            conditions.append('(r."lastLogId" IS NULL OR m."logId" <= r."lastLogId")')
        rows = self._run_query(
            path,
            key,
            f'''
            WITH ranked AS (
                SELECT m."chatId" AS "chatId",
                       m."logId" AS "logId",
                       {msg_id_expr} AS "msgId",
                       {revision_expr} AS "revision",
                       ROW_NUMBER() OVER (
                           PARTITION BY m."chatId"
                           ORDER BY m."logId" DESC,
                                    {msg_id_expr} DESC,
                                    {revision_expr} DESC
                       ) AS "rowNumber"
                FROM NTChatMessage m
                {joins}
                WHERE {" AND ".join(conditions)}
            )
            SELECT "chatId", "logId", "msgId", "revision"
            FROM ranked
            WHERE "rowNumber" = 1
            ORDER BY "chatId" ASC;
            ''',
            params=room_params,
            max_rows=len(normalized_room_ids),
        )

        result: dict[int, str | None] = {room_id: None for room_id in normalized_room_ids}
        for row in rows:
            ordering_key = self._ordering_key(row)
            room_id = ordering_key[0]
            if room_id not in result:
                raise ServiceError("KakaoTalk database returned an unexpected room ID")
            result[room_id] = encode_cursor(ordering_key, scope=room_id)
        return result

    @staticmethod
    def _packed_cursor_key(cursor: Any, *, room_id: int) -> str:
        if cursor is None or cursor == "":
            return ""
        decoded = decode_cursor(cursor)
        if decoded is None or decoded[1] != room_id:
            raise ValidationError("cursor room scope does not match room_id")
        return "".join(f"{value:019d}" for value in decoded[0])

    def updates_for_rooms(
        self,
        *,
        room_cursors: Mapping[Any, Any],
        limit: Any = DEFAULT_UPDATE_LIMIT,
    ) -> dict[str, Any]:
        """Read one bounded global page using independent room watermarks."""

        bounded = validate_limit(limit, default=DEFAULT_UPDATE_LIMIT, maximum=MAX_UPDATE_LIMIT)
        if not isinstance(room_cursors, Mapping):
            raise ValidationError("global room cursors must be a mapping")
        if len(room_cursors) > MAX_GLOBAL_ROOMS:
            raise ValidationError("too many KakaoTalk rooms for one global poll")

        normalized_cursors: list[tuple[int, str]] = []
        seen_room_ids: set[int] = set()
        for raw_room_id, cursor in room_cursors.items():
            room_id = validate_id(raw_room_id, "room_id")
            if room_id is None or room_id in seen_room_ids:
                raise ValidationError("global room IDs are invalid")
            seen_room_ids.add(room_id)
            normalized_cursors.append(
                (room_id, self._packed_cursor_key(cursor, room_id=room_id))
            )
        normalized_cursors.sort()
        if not normalized_cursors:
            return {"hasMore": False, "messages": []}

        # Two bound parameters per room keep the query below the stock
        # SQLite/SQLCipher variable limit even at MAX_GLOBAL_ROOMS: one room
        # ID and one fixed-width numeric watermark.  No cursor text is SQL.
        if 2 * len(normalized_cursors) + 1 > 999:
            raise ServiceError("global polling query is too large")

        path, key = self._connection()
        select_parts, msg_id_expr, revision_expr, joins, has_last_log_id = self._updates_projection(path, key)
        params: dict[str, Any] = {"fetch_limit": bounded + 1}
        room_conditions: list[str] = []
        for index, (room_id, packed_cursor) in enumerate(normalized_cursors):
            room_name = f"room_{index}"
            watermark_name = f"watermark_{index}"
            params[room_name] = room_id
            params[watermark_name] = packed_cursor
            watermark = f":{watermark_name}"
            cursor_condition = f"""
                (
                    {watermark} = ''
                    OR m."logId" > CAST(substr({watermark}, 20, 19) AS INTEGER)
                    OR (
                        m."logId" = CAST(substr({watermark}, 20, 19) AS INTEGER)
                        AND {msg_id_expr} > CAST(substr({watermark}, 39, 19) AS INTEGER)
                    )
                    OR (
                        m."logId" = CAST(substr({watermark}, 20, 19) AS INTEGER)
                        AND {msg_id_expr} = CAST(substr({watermark}, 39, 19) AS INTEGER)
                        AND {revision_expr} > CAST(substr({watermark}, 58, 19) AS INTEGER)
                    )
                )
            """
            room_condition = f'(m."chatId" = :{room_name} AND {cursor_condition})'
            if has_last_log_id:
                room_condition = (
                    f'{room_condition} AND '
                    '(r."lastLogId" IS NULL OR m."logId" <= r."lastLogId")'
                )
            room_conditions.append(room_condition)

        rows = self._run_query(
            path,
            key,
            f'''
            SELECT {", ".join(select_parts)}
            FROM NTChatMessage m
            {joins}
            WHERE {" OR ".join(room_conditions)}
            ORDER BY m."chatId" ASC, m."logId" ASC,
                     {msg_id_expr} ASC, {revision_expr} ASC
            LIMIT :fetch_limit;
            ''',
            params=params,
            max_rows=bounded + 1,
        )
        rows.sort(key=self._ordering_key)
        has_more = len(rows) > bounded
        page_rows = rows[:bounded]
        messages: list[dict[str, Any]] = []
        for row in page_rows:
            ordering_key = self._ordering_key(row)
            normalized_row = dict(row)
            if normalized_row.get("msgId") is None:
                normalized_row["msgId"] = ordering_key[2]
            if normalized_row.get("revision") is None:
                normalized_row["revision"] = ordering_key[3]
            messages.append(normalize_message(normalized_row))

        while messages:
            envelope = {"hasMore": has_more, "messages": messages}
            if len(json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")) <= MAX_OUTPUT_BYTES:
                return envelope
            messages.pop()
            has_more = True
        if page_rows:
            raise ServiceError("result exceeds the output limit; reduce the requested limit")
        return {"hasMore": False, "messages": []}

    def high_water_cursor(self, *, room_id: Any = None) -> str | None:
        """Return the latest numeric cursor without selecting message contents."""

        validated_room_id = validate_id(room_id, "room_id", allow_none=True)
        path, key = self._connection()
        message_columns = self._table_columns(path, key, "NTChatMessage")
        if not {"chatid", "logid"}.issubset(message_columns):
            raise ServiceError("KakaoTalk message schema is unavailable")

        try:
            room_columns = self._table_columns(path, key, "NTChatRoom")
        except ServiceError:
            room_columns = set()

        msg_id_expr = (
            'COALESCE(m."msgId", m."logId")'
            if "msgid" in message_columns
            else 'm."logId"'
        )
        revision_expr = (
            'COALESCE(m."revision", 0)'
            if "revision" in message_columns
            else "0"
        )
        joins = ""
        has_last_log_id = False
        if "chatid" in room_columns:
            joins = 'LEFT JOIN NTChatRoom r ON m."chatId" = r."chatId"'
            has_last_log_id = "lastlogid" in room_columns

        conditions: list[str] = []
        params: dict[str, Any] = {"fetch_limit": 1}
        if validated_room_id is not None:
            conditions.append('m."chatId" = :room_id')
            params["room_id"] = validated_room_id
        if has_last_log_id:
            conditions.append('(r."lastLogId" IS NULL OR m."logId" <= r."lastLogId")')
        where = "WHERE " + " AND ".join(conditions) if conditions else ""
        rows = self._run_query(
            path,
            key,
            f"""
            SELECT m."chatId" AS "chatId",
                   m."logId" AS "logId",
                   {msg_id_expr} AS "msgId",
                   {revision_expr} AS "revision"
            FROM NTChatMessage m
            {joins}
            {where}
            ORDER BY m."chatId" DESC, m."logId" DESC,
                     {msg_id_expr} DESC, {revision_expr} DESC
            LIMIT :fetch_limit;
            """,
            params=params,
            max_rows=1,
        )
        if not rows:
            return None
        return encode_cursor(self._ordering_key(rows[0]), scope=validated_room_id)

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

    def resolve_room_identity(
        self,
        *,
        room_id: Any,
        display_identity: Any,
    ) -> dict[str, Any] | None:
        """Resolve one room only when its exact display identity is provably unique."""

        validated_room_id = validate_id(room_id, "room_id")
        if (
            not isinstance(display_identity, str)
            or not display_identity
            or "\x00" in display_identity
            or len(display_identity.encode("utf-8")) > MAX_ROOM_NAME_BYTES
        ):
            raise ValidationError("display_identity is invalid")

        path, key = self._connection()
        rows = self._run_query(
            path,
            key,
            """
            WITH room_identities AS (
                SELECT r.chatId, r.type, r.chatName, r.linkId, r.lastLogId,
                       r.directChatMemberUserId, r.activeMembersCount,
                       COALESCE(
                           NULLIF(r.chatName, ''),
                           NULLIF(du.nickName, ''),
                           NULLIF((
                               SELECT GROUP_CONCAT(member.nickName, ', ')
                               FROM (
                                   SELECT u.nickName
                                   FROM (
                                       SELECT DISTINCT m.authorId
                                       FROM NTChatMessage m
                                       WHERE m.chatId = r.chatId
                                       LIMIT 20
                                   ) authors
                                   JOIN NTUser u ON authors.authorId = u.userId
                                                AND u.linkId = COALESCE(r.linkId, 0)
                                   WHERE u.nickName IS NOT NULL AND u.nickName != ''
                                   ORDER BY u.nickName COLLATE NOCASE
                                   LIMIT 4
                               ) member
                           ), ''),
                           '(unnamed)'
                       ) AS displayName
                FROM NTChatRoom r
                LEFT JOIN NTUser du ON r.directChatMemberUserId = du.userId
                                   AND du.linkId = COALESCE(r.linkId, 0)
            )
            SELECT chatId, type, chatName, linkId, lastLogId,
                   directChatMemberUserId, activeMembersCount, displayName
            FROM room_identities
            WHERE chatId = :room_id OR displayName = :display_identity
            ORDER BY CASE WHEN chatId = :room_id THEN 0 ELSE 1 END, chatId ASC
            LIMIT :limit;
            """,
            params={
                "room_id": validated_room_id,
                "display_identity": display_identity,
                "limit": 3,
            },
            max_rows=3,
        )
        if not isinstance(rows, list) or len(rows) > 3 or any(not isinstance(row, Mapping) for row in rows):
            raise ServiceError("KakaoTalk room identity is unavailable")

        target_rows = [
            row
            for row in rows
            if type(row.get("chatId")) is int and row.get("chatId") == validated_room_id
        ]
        identity_rows = [
            row
            for row in rows
            if isinstance(row.get("displayName"), str)
            and row.get("displayName") == display_identity
        ]
        if (
            len(target_rows) != 1
            or len(identity_rows) != 1
            or target_rows[0] is not identity_rows[0]
        ):
            return None
        return normalize_room(target_rows[0])

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
        return [normalize_message(row, extended=False) for row in rows]

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
        return [normalize_message(row, extended=False) for row in rows]

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
        return [normalize_message(row, extended=False) for row in rows]

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

        target = normalize_message(target_rows[0], extended=False)
        return {
            "chatId": validated_room_id,
            "messageId": validated_message_id,
            "before": [normalize_message(row, extended=False) for row in before_rows],
            "message": target,
            "after": [normalize_message(row, extended=False) for row in after_rows],
        }
