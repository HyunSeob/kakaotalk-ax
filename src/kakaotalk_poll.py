"""Durable, one-shot local polling for the read-only KakaoTalk service.

This module owns only polling state and acknowledgement flow.  It never opens
the database itself and delegates reads to the bounded V2 service methods.
"""

from __future__ import annotations

import json
import os
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
    validate_id,
    validate_limit,
)


DEFAULT_STATE_FILE = os.path.expanduser(
    "~/.config/kakaotalk-reader/poll-state.json"
)
STATE_VERSION = 1
MAX_STATE_BYTES = 4_096
MAX_STATE_INTEGER = 9_223_372_036_854_775_807


class PollingError(RuntimeError):
    """A safe, bounded polling/state error suitable for a CLI boundary."""


class PollingStateError(PollingError):
    """The durable polling state is missing, malformed, or unsafe to use."""


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
