"""Durable, one-shot local polling for the read-only KakaoTalk service.

This module owns only polling state and acknowledgement flow.  It never opens
the database itself and delegates reads to the bounded V2 service methods.
"""

from __future__ import annotations

import base64
import json
import os
import re
import stat
import tempfile
from collections.abc import Mapping
from typing import Any

from kakaotalk_service import (
    DEFAULT_UPDATE_LIMIT,
    MAX_OUTPUT_BYTES,
    MAX_UPDATE_LIMIT,
    KakaoTalkService,
    ServiceError,
    ValidationError,
    decode_cursor,
    encode_cursor,
    validate_id,
    validate_limit,
)


DEFAULT_STATE_FILE = os.path.expanduser(
    "~/.config/kakaotalk-reader/poll-state.json"
)
STATE_VERSION = 1
MAX_STATE_BYTES = 32_768
MAX_STATE_INTEGER = 9_223_372_036_854_775_807
GLOBAL_CURSOR_PREFIX = "ktg1."
GLOBAL_CURSOR_VERSION = 1
MAX_GLOBAL_ROOMS = 100
MAX_GLOBAL_CURSOR_JSON_BYTES = 24_576
MAX_GLOBAL_CURSOR_CHARS = 32_768


class PollingError(RuntimeError):
    """A safe, bounded polling/state error suitable for a CLI boundary."""


class PollingStateError(PollingError):
    """The durable polling state is missing, malformed, or unsafe to use."""


def _encode_global_cursor(room_cursors: Mapping[int, str | None]) -> str:
    """Encode bounded per-room numeric cursors for an unscoped poll."""

    if len(room_cursors) > MAX_GLOBAL_ROOMS:
        raise ValueError("global polling tracks too many rooms")

    rooms: list[list[Any]] = []
    for raw_room_id in sorted(room_cursors):
        room_id = validate_id(raw_room_id, "room_id")
        cursor = room_cursors[raw_room_id]
        ordering_key: list[int] | None = None
        if cursor is not None:
            decoded = decode_cursor(cursor)
            if decoded is None or decoded[1] != room_id:
                raise ValueError("global polling cursor scope is invalid")
            ordering_key = list(decoded[0])
        rooms.append([room_id, ordering_key])

    payload = {"r": rooms, "v": GLOBAL_CURSOR_VERSION}
    raw = json.dumps(
        payload,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    if len(raw) > MAX_GLOBAL_CURSOR_JSON_BYTES:
        raise ValueError("global polling cursor is too large")
    encoded = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
    result = GLOBAL_CURSOR_PREFIX + encoded
    if len(result) > MAX_GLOBAL_CURSOR_CHARS:
        raise ValueError("global polling cursor is too large")
    return result


def _decode_global_cursor(value: Any) -> dict[int, str | None]:
    """Decode and validate a V2.1 global per-room cursor."""

    if not isinstance(value, str) or not value.startswith(GLOBAL_CURSOR_PREFIX):
        if isinstance(value, str) and value.startswith("kt2."):
            raise PollingStateError(
                "global polling state uses the legacy cursor; remove it to bootstrap V2.1 again"
            )
        raise ValueError("global polling cursor is invalid")
    if len(value) > MAX_GLOBAL_CURSOR_CHARS:
        raise ValueError("global polling cursor is invalid")

    encoded = value[len(GLOBAL_CURSOR_PREFIX) :]
    if not encoded or not re.fullmatch(r"[A-Za-z0-9_-]+", encoded):
        raise ValueError("global polling cursor is invalid")
    try:
        raw = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
        if len(raw) > MAX_GLOBAL_CURSOR_JSON_BYTES:
            raise ValueError("global polling cursor is too large")
        canonical = base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")
        if canonical != encoded:
            raise ValueError("global polling cursor is not canonical")
        payload = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=_reject_json_constant,
        )
    except (ValueError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("global polling cursor is invalid") from exc

    if not isinstance(payload, Mapping) or set(payload) != {"r", "v"}:
        raise ValueError("global polling cursor is invalid")
    version = payload["v"]
    if (
        isinstance(version, bool)
        or not isinstance(version, int)
        or version != GLOBAL_CURSOR_VERSION
    ):
        raise ValueError("global polling cursor version is unsupported")
    rooms = payload["r"]
    if not isinstance(rooms, list) or len(rooms) > MAX_GLOBAL_ROOMS:
        raise ValueError("global polling cursor room set is invalid")

    normalized: dict[int, str | None] = {}
    previous_room_id = 0
    for item in rooms:
        if not isinstance(item, list) or len(item) != 2:
            raise ValueError("global polling cursor room entry is invalid")
        room_id = validate_id(item[0], "room_id")
        if room_id <= previous_room_id or room_id in normalized:
            raise ValueError("global polling cursor rooms are not ordered")
        previous_room_id = room_id
        ordering_key = item[1]
        if ordering_key is None:
            normalized[room_id] = None
            continue
        if not isinstance(ordering_key, list) or len(ordering_key) != 4:
            raise ValueError("global polling cursor ordering key is invalid")
        normalized[room_id] = encode_cursor(tuple(ordering_key), scope=room_id)
    return normalized


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate state key")
        result[key] = value
    return result


def _reject_json_constant(_value: str) -> Any:
    raise ValueError("non-finite state value")


def _bounded_integer(value: Any) -> int:
    if (
        isinstance(value, bool)
        or not isinstance(value, int)
        or value < 0
        or value > MAX_STATE_INTEGER
    ):
        raise ValueError("state integer is invalid")
    return value


def _normalize_signature(value: Any) -> dict[str, list[int] | None]:
    if not isinstance(value, Mapping) or set(value) != {"database", "wal"}:
        raise ValueError("state signature is invalid")

    normalized: dict[str, list[int] | None] = {}
    for field in ("database", "wal"):
        signature = value[field]
        if signature is None:
            normalized[field] = None
            continue
        if not isinstance(signature, (list, tuple)) or len(signature) != 2:
            raise ValueError("state signature is invalid")
        normalized[field] = [_bounded_integer(signature[0]), _bounded_integer(signature[1])]
    return normalized


def _normalize_cursor(value: Any, scope: int | None) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str) or not value:
        raise ValueError("state cursor is invalid")
    if scope is None:
        try:
            _decode_global_cursor(value)
        except PollingStateError:
            raise
        except (TypeError, ValueError) as exc:
            raise ValueError("state cursor is invalid") from exc
        return value
    try:
        decoded = decode_cursor(value)
    except ValidationError as exc:
        raise ValueError("state cursor is invalid") from exc
    if decoded is None or decoded[1] != scope:
        raise ValueError("state cursor scope is invalid")
    return value


def _normalize_state(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping) or set(value) != {
        "v",
        "scope",
        "cursor",
        "signature",
        "pending",
        "more",
    }:
        raise ValueError("state envelope is invalid")
    version = value["v"]
    if isinstance(version, bool) or not isinstance(version, int) or version != STATE_VERSION:
        raise ValueError("state version is invalid")
    scope = validate_id(value["scope"], "room_id", allow_none=True)
    cursor = _normalize_cursor(value["cursor"], scope)
    signature = _normalize_signature(value["signature"])

    more = value["more"]
    if not isinstance(more, bool):
        raise ValueError("state continuation is invalid")

    pending_value = value["pending"]
    pending: dict[str, Any] | None
    if pending_value is None:
        pending = None
    else:
        if not isinstance(pending_value, Mapping) or set(pending_value) != {
            "cursor",
            "signature",
            "hasMore",
        }:
            raise ValueError("state pending batch is invalid")
        pending_cursor = _normalize_cursor(pending_value["cursor"], scope)
        if pending_cursor is None or not isinstance(pending_value["hasMore"], bool):
            raise ValueError("state pending batch is invalid")
        pending = {
            "cursor": pending_cursor,
            "signature": _normalize_signature(pending_value["signature"]),
            "hasMore": pending_value["hasMore"],
        }

    if pending is None and more and cursor is None:
        raise ValueError("state continuation is invalid")
    return {
        "v": STATE_VERSION,
        "scope": scope,
        "cursor": cursor,
        "signature": signature,
        "pending": pending,
        "more": more,
    }


def _read_state(path: str) -> dict[str, Any] | None:
    try:
        file_stat = os.lstat(path)
    except FileNotFoundError:
        return None
    except OSError as exc:
        raise PollingStateError("polling state is unavailable") from exc

    if stat.S_ISLNK(file_stat.st_mode) or not stat.S_ISREG(file_stat.st_mode):
        raise PollingStateError("polling state is invalid")
    _ensure_state_directory(path)
    if stat.S_IMODE(file_stat.st_mode) != 0o600:
        try:
            os.chmod(path, 0o600)
        except OSError as exc:
            raise PollingStateError("polling state is unavailable") from exc
    if file_stat.st_size > MAX_STATE_BYTES:
        raise PollingStateError("polling state is invalid")
    try:
        with open(path, "rb") as state_file:
            raw = state_file.read(MAX_STATE_BYTES + 1)
    except OSError as exc:
        raise PollingStateError("polling state is unavailable") from exc
    if len(raw) > MAX_STATE_BYTES:
        raise PollingStateError("polling state is invalid")
    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
            parse_constant=_reject_json_constant,
        )
        return _normalize_state(value)
    except PollingStateError:
        raise
    except Exception as exc:
        raise PollingStateError("polling state is invalid") from exc


def _ensure_state_directory(path: str) -> str:
    parent = os.path.abspath(os.path.dirname(path) or ".")
    try:
        os.makedirs(parent, mode=0o700, exist_ok=True)
        os.chmod(parent, 0o700)
    except OSError as exc:
        raise PollingStateError("polling state directory is unavailable") from exc
    return parent


def _write_state(path: str, value: Mapping[str, Any]) -> None:
    try:
        state = _normalize_state(value)
        encoded = json.dumps(
            state,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    except (TypeError, ValueError, ValidationError) as exc:
        raise PollingStateError("polling state is invalid") from exc
    if len(encoded) + 1 > MAX_STATE_BYTES:
        raise PollingStateError("polling state is invalid")

    parent = _ensure_state_directory(path)
    try:
        if os.path.lexists(path) and os.path.islink(path):
            raise OSError("state path is a symbolic link")
        fd, temporary_path = tempfile.mkstemp(
            prefix=".poll-state.",
            suffix=".tmp",
            dir=parent,
        )
        try:
            os.fchmod(fd, 0o600)
            with os.fdopen(fd, "wb") as state_file:
                state_file.write(encoded)
                state_file.write(b"\n")
                state_file.flush()
                os.fsync(state_file.fileno())
            os.replace(temporary_path, path)
            os.chmod(path, 0o600)
            try:
                directory_fd = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
            except OSError:
                directory_fd = None
            if directory_fd is not None:
                try:
                    try:
                        os.fsync(directory_fd)
                    except OSError:
                        pass
                finally:
                    os.close(directory_fd)
        finally:
            if os.path.exists(temporary_path):
                os.unlink(temporary_path)
    except PollingStateError:
        raise
    except OSError as exc:
        raise PollingStateError("polling state could not be saved") from exc


def _signature_for_state(value: Any) -> dict[str, list[int] | None]:
    try:
        return _normalize_signature(value)
    except (TypeError, ValueError) as exc:
        raise PollingError("KakaoTalk source status is invalid") from exc


def _empty_state(*, scope: int | None, cursor: str | None, signature: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "v": STATE_VERSION,
        "scope": scope,
        "cursor": cursor,
        "signature": _signature_for_state(signature),
        "pending": None,
        "more": False,
    }


def _poll_envelope(
    *,
    state: str,
    messages: list[dict[str, Any]],
    has_more: bool,
    next_cursor: str | None,
    ack_required: bool,
    ack_token: str | None,
) -> dict[str, Any]:
    envelope = {
        "state": state,
        "messages": messages,
        "hasMore": bool(has_more),
        "nextCursor": next_cursor,
        "ackRequired": bool(ack_required),
        "ackToken": ack_token,
    }
    try:
        encoded = json.dumps(envelope, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise PollingError("polling result is invalid") from exc
    if len(encoded) > MAX_OUTPUT_BYTES:
        raise PollingError("polling result exceeds the output limit")
    return envelope


class KakaoTalkPoller:
    """Run one bounded poll and commit a batch only through explicit ack."""

    def __init__(
        self,
        *,
        service: KakaoTalkService | None = None,
        state_file: str | os.PathLike[str] = DEFAULT_STATE_FILE,
    ) -> None:
        self.service = service or KakaoTalkService()
        self.state_file = os.fspath(state_file)

    def _source_signature(self) -> dict[str, list[int] | None]:
        try:
            return _signature_for_state(self.service.source_signature())
        except (ValidationError, PollingError):
            raise
        except ServiceError as exc:
            raise PollingError("KakaoTalk source status is unavailable") from exc
        except Exception as exc:
            raise PollingError("KakaoTalk source status is unavailable") from exc

    @staticmethod
    def _scope(room_id: Any) -> int | None:
        try:
            return validate_id(room_id, "room_id", allow_none=True)
        except ValidationError:
            raise

    @staticmethod
    def _update_result(
        result: Any,
        *,
        scope: int | None,
        committed_cursor: str | None,
    ) -> tuple[list[dict[str, Any]], bool, str | None]:
        if not isinstance(result, Mapping):
            raise PollingError("KakaoTalk update result is invalid")
        messages = result.get("messages")
        has_more = result.get("hasMore")
        next_cursor = result.get("nextCursor")
        if not isinstance(messages, list) or not isinstance(has_more, bool):
            raise PollingError("KakaoTalk update result is invalid")
        if next_cursor is not None:
            try:
                decoded = decode_cursor(next_cursor)
            except ValidationError as exc:
                raise PollingError("KakaoTalk update result is invalid") from exc
            if decoded is None or decoded[1] != scope:
                raise PollingError("KakaoTalk update result is invalid")
        if not messages and next_cursor is None:
            next_cursor = committed_cursor
        if messages and next_cursor is None:
            raise PollingError("KakaoTalk update result is invalid")
        return messages, has_more, next_cursor

    def _global_room_ids(self) -> list[int]:
        try:
            room_ids = self.service.room_ids(limit=MAX_GLOBAL_ROOMS)
        except (ValidationError, ServiceError) as exc:
            raise PollingError("KakaoTalk room discovery failed") from exc
        if not isinstance(room_ids, list) or len(room_ids) > MAX_GLOBAL_ROOMS:
            raise PollingError("KakaoTalk room discovery result is invalid")
        normalized: list[int] = []
        for room_id in room_ids:
            try:
                normalized_id = validate_id(room_id, "room_id")
            except ValidationError as exc:
                raise PollingError("KakaoTalk room discovery result is invalid") from exc
            normalized.append(normalized_id)
        return sorted(set(normalized))

    @staticmethod
    def _message_cursor(message: Any, *, room_id: int) -> tuple[tuple[int, int, int, int], str]:
        if not isinstance(message, Mapping):
            raise PollingError("KakaoTalk update result is invalid")
        try:
            chat_id = validate_id(message.get("chatId"), "chatId")
            log_id = validate_id(message.get("logId"), "logId")
            msg_id = message.get("msgId")
            if msg_id is None:
                msg_id = log_id
            revision = message.get("revision")
            if revision is None:
                revision = 0
            cursor = encode_cursor((chat_id, log_id, msg_id, revision), scope=room_id)
        except (ValidationError, TypeError) as exc:
            raise PollingError("KakaoTalk update result is invalid") from exc
        if chat_id != room_id:
            raise PollingError("KakaoTalk update result is invalid")
        decoded = decode_cursor(cursor)
        if decoded is None:
            raise PollingError("KakaoTalk update result is invalid")
        return decoded[0], cursor

    def _poll_global(
        self,
        *,
        state: dict[str, Any] | None,
        signature: dict[str, list[int] | None],
        bounded_limit: int,
    ) -> dict[str, Any]:
        committed_cursors = (
            _decode_global_cursor(state["cursor"])
            if state is not None and state["cursor"] is not None
            else {}
        )

        if state is None:
            room_cursors: dict[int, str | None] = {}
            for room_id in self._global_room_ids():
                try:
                    room_cursors[room_id] = self.service.high_water_cursor(room_id=room_id)
                except (ValidationError, ServiceError) as exc:
                    raise PollingError("KakaoTalk bootstrap read failed") from exc
            cursor = _encode_global_cursor(room_cursors)
            state = _empty_state(scope=None, cursor=cursor, signature=signature)
            _write_state(self.state_file, state)
            return _poll_envelope(
                state="bootstrapped",
                messages=[],
                has_more=False,
                next_cursor=cursor,
                ack_required=False,
                ack_token=None,
            )

        pending = state["pending"]
        pending_cursors = (
            _decode_global_cursor(pending["cursor"])
            if pending is not None
            else {}
        )
        needs_global_upgrade = state["scope"] is None and state["cursor"] is None
        should_read = (
            pending is not None
            or state["more"]
            or state["signature"] != signature
            or needs_global_upgrade
        )
        if not should_read:
            return _poll_envelope(
                state="unchanged",
                messages=[],
                has_more=False,
                next_cursor=state["cursor"],
                ack_required=False,
                ack_token=None,
            )

        discovered_room_ids = self._global_room_ids()
        room_cursors = dict(committed_cursors)
        for room_id, cursor in pending_cursors.items():
            room_cursors.setdefault(room_id, cursor)
        new_rooms = [room_id for room_id in discovered_room_ids if room_id not in room_cursors]
        bootstrapped_rooms: set[int] = set()
        for room_id in new_rooms:
            if needs_global_upgrade:
                try:
                    room_cursors[room_id] = self.service.high_water_cursor(room_id=room_id)
                except (ValidationError, ServiceError) as exc:
                    raise PollingError("KakaoTalk room bootstrap read failed") from exc
                if room_cursors[room_id] is not None:
                    bootstrapped_rooms.add(room_id)
            else:
                # A room absent from the initial room set was not part of the
                # historical bootstrap. Start it at the empty cursor so its
                # first post-discovery row cannot be skipped.
                room_cursors[room_id] = None

        room_ids = sorted(room_cursors)
        room_has_more: dict[int, bool] = {}
        entries: list[tuple[tuple[int, int, int, int], int, dict[str, Any], str]] = []
        for room_id in room_ids:
            if room_id in bootstrapped_rooms:
                continue
            try:
                raw_result = self.service.updates(
                    cursor=room_cursors[room_id] or "",
                    limit=bounded_limit,
                    room_id=room_id,
                )
            except (ValidationError, ServiceError) as exc:
                raise PollingError("KakaoTalk update read failed") from exc
            messages, has_more, _ = self._update_result(
                raw_result,
                scope=room_id,
                committed_cursor=room_cursors[room_id],
            )
            room_has_more[room_id] = has_more
            for message in messages:
                ordering_key, message_cursor = self._message_cursor(message, room_id=room_id)
                entries.append((ordering_key, room_id, message, message_cursor))

        entries.sort(key=lambda item: (item[0], item[1]))
        if not entries:
            if pending is not None:
                pending_cursor = _encode_global_cursor(room_cursors)
                if pending_cursor != pending["cursor"]:
                    pending = {
                        "cursor": pending_cursor,
                        "signature": signature,
                        "hasMore": pending["hasMore"],
                    }
                    state["pending"] = pending
                    _write_state(self.state_file, state)
                return _poll_envelope(
                    state="updated",
                    messages=[],
                    has_more=pending["hasMore"],
                    next_cursor=pending["cursor"],
                    ack_required=True,
                    ack_token=pending["cursor"],
                )

            cursor = _encode_global_cursor(room_cursors)
            state["cursor"] = cursor
            state["signature"] = signature
            state["more"] = False
            _write_state(self.state_file, state)
            return _poll_envelope(
                state="updated",
                messages=[],
                has_more=False,
                next_cursor=cursor,
                ack_required=False,
                ack_token=None,
            )

        max_count = min(bounded_limit, len(entries))
        for selected_count in range(max_count, 0, -1):
            selected = entries[:selected_count]
            next_room_cursors = dict(room_cursors)
            for _, room_id, _, message_cursor in selected:
                next_room_cursors[room_id] = message_cursor
            has_more = selected_count < len(entries) or any(room_has_more.values())
            next_cursor = _encode_global_cursor(next_room_cursors)
            try:
                envelope = _poll_envelope(
                    state="updated",
                    messages=[message for _, _, message, _ in selected],
                    has_more=has_more,
                    next_cursor=next_cursor,
                    ack_required=True,
                    ack_token=next_cursor,
                )
            except PollingError:
                if selected_count == 1:
                    raise
                continue

            state["pending"] = {
                "cursor": next_cursor,
                "signature": signature,
                "hasMore": has_more,
            }
            _write_state(self.state_file, state)
            return envelope

        raise PollingError("polling result exceeds the output limit")

    def poll_once(
        self,
        *,
        limit: Any = DEFAULT_UPDATE_LIMIT,
        room_id: Any = None,
    ) -> dict[str, Any]:
        bounded_limit = validate_limit(
            limit,
            default=DEFAULT_UPDATE_LIMIT,
            maximum=MAX_UPDATE_LIMIT,
        )
        scope = self._scope(room_id)
        state = _read_state(self.state_file)
        if state is not None and state["scope"] != scope:
            raise PollingStateError("polling state scope does not match room_id")
        signature = self._source_signature()

        if scope is None:
            return self._poll_global(
                state=state,
                signature=signature,
                bounded_limit=bounded_limit,
            )

        if state is None:
            try:
                cursor = self.service.high_water_cursor(room_id=scope)
            except (ValidationError, ServiceError) as exc:
                raise PollingError("KakaoTalk bootstrap read failed") from exc
            state = _empty_state(scope=scope, cursor=cursor, signature=signature)
            _write_state(self.state_file, state)
            return _poll_envelope(
                state="bootstrapped",
                messages=[],
                has_more=False,
                next_cursor=cursor,
                ack_required=False,
                ack_token=None,
            )

        pending = state["pending"]
        should_read = pending is not None or state["more"] or state["signature"] != signature
        if not should_read:
            return _poll_envelope(
                state="unchanged",
                messages=[],
                has_more=False,
                next_cursor=state["cursor"],
                ack_required=False,
                ack_token=None,
            )

        committed_cursor = state["cursor"]
        query_cursor = committed_cursor or ""
        try:
            raw_result = self.service.updates(
                cursor=query_cursor,
                limit=bounded_limit,
                room_id=scope,
            )
        except (ValidationError, ServiceError) as exc:
            raise PollingError("KakaoTalk update read failed") from exc
        messages, has_more, next_cursor = self._update_result(
            raw_result,
            scope=scope,
            committed_cursor=committed_cursor,
        )

        if messages:
            if next_cursor == committed_cursor:
                raise PollingError("KakaoTalk update result did not advance")
            state["pending"] = {
                "cursor": next_cursor,
                "signature": signature,
                "hasMore": has_more,
            }
            _write_state(self.state_file, state)
            return _poll_envelope(
                state="updated",
                messages=messages,
                has_more=has_more,
                next_cursor=next_cursor,
                ack_required=True,
                ack_token=next_cursor,
            )

        if pending is not None:
            return _poll_envelope(
                state="updated",
                messages=[],
                has_more=pending["hasMore"],
                next_cursor=pending["cursor"],
                ack_required=True,
                ack_token=pending["cursor"],
            )

        state["signature"] = signature
        state["more"] = False
        _write_state(self.state_file, state)
        return _poll_envelope(
            state="updated",
            messages=[],
            has_more=False,
            next_cursor=state["cursor"],
            ack_required=False,
            ack_token=None,
        )

    def acknowledge(self, token: Any, *, room_id: Any = None) -> dict[str, Any]:
        scope = self._scope(room_id)
        state = _read_state(self.state_file)
        if state is None or state["scope"] != scope:
            raise PollingStateError("polling state is not ready to acknowledge")
        pending = state["pending"]
        if pending is None or not isinstance(token, str) or token != pending["cursor"]:
            raise PollingStateError("acknowledgement does not match the pending batch")

        state["cursor"] = pending["cursor"]
        state["signature"] = pending["signature"]
        state["more"] = pending["hasMore"]
        state["pending"] = None
        _write_state(self.state_file, state)
        return _poll_envelope(
            state="acknowledged",
            messages=[],
            has_more=state["more"],
            next_cursor=state["cursor"],
            ack_required=False,
            ack_token=None,
        )


def poll_once(
    *,
    service: KakaoTalkService | None = None,
    state_file: str | os.PathLike[str] = DEFAULT_STATE_FILE,
    limit: Any = DEFAULT_UPDATE_LIMIT,
    room_id: Any = None,
) -> dict[str, Any]:
    """Convenience API for one scheduled polling tick."""

    return KakaoTalkPoller(service=service, state_file=state_file).poll_once(
        limit=limit,
        room_id=room_id,
    )


def acknowledge(
    token: Any,
    *,
    service: KakaoTalkService | None = None,
    state_file: str | os.PathLike[str] = DEFAULT_STATE_FILE,
    room_id: Any = None,
) -> dict[str, Any]:
    """Convenience API for committing a successfully processed batch."""

    return KakaoTalkPoller(service=service, state_file=state_file).acknowledge(
        token,
        room_id=room_id,
    )
