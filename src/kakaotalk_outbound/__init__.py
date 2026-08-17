"""Guarded, opt-in outbound coordination kept separate from the read plane."""

from __future__ import annotations

import secrets
import threading
import time
import unicodedata
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Callable, Protocol


CONFIRMATION_VALUE = "CONFIRM SEND"
MAX_ROOM_IDENTITY_CHARS = 200
MAX_ROOM_IDENTITY_BYTES = 512
MAX_TEXT_CHARS = 2_000
MAX_TEXT_BYTES = 6_000
MAX_DRAFT_ID_CHARS = 128
DEFAULT_DRAFT_TTL_SECONDS = 60
DEFAULT_MAX_ACTIVE_DRAFTS = 128


class OutboundError(ValueError):
    """A bounded outbound refusal safe for mapping to a generic response."""

    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


class DraftError(OutboundError):
    """The supplied draft cannot be used."""


@dataclass(frozen=True)
class AXAvailability:
    is_available: bool

    @classmethod
    def available(cls) -> "AXAvailability":
        return cls(True)

    @classmethod
    def unavailable(cls) -> "AXAvailability":
        return cls(False)


@dataclass(frozen=True)
class AXPreparation:
    status: str
    receipt: str | None = None

    @classmethod
    def ready(cls, receipt: str) -> "AXPreparation":
        return cls("ready", receipt)

    @classmethod
    def mismatch(cls) -> "AXPreparation":
        return cls("mismatch")

    @classmethod
    def readback_mismatch(cls) -> "AXPreparation":
        return cls("readback_mismatch")

    @classmethod
    def unavailable(cls) -> "AXPreparation":
        return cls("unavailable")


@dataclass(frozen=True)
class AXSendOutcome:
    status: str

    @classmethod
    def invoked(cls) -> "AXSendOutcome":
        return cls("invoked")

    @classmethod
    def mismatch(cls) -> "AXSendOutcome":
        return cls("mismatch")

    @classmethod
    def unavailable(cls) -> "AXSendOutcome":
        return cls("unavailable")


class AXAdapter(Protocol):
    def check_availability(self) -> AXAvailability: ...

    def prepare_text(self, *, expected_identity: str, text: str) -> AXPreparation: ...

    def send_prepared(self, preparation: AXPreparation) -> AXSendOutcome: ...


@dataclass(frozen=True)
class Draft:
    draft_id: str
    room_id: int
    target_identity: str
    text: str
    expires_at: float


@dataclass(frozen=True)
class Preview:
    draft_id: str
    target_identity: str
    text: str
    expires_in_seconds: int
    confirmation_value: str = CONFIRMATION_VALUE


@dataclass(frozen=True)
class OutboundResult:
    status: str
    room_id: int
    verification_path: str


class DraftStore:
    """Process-local, monotonic, one-time draft storage."""

    def __init__(
        self,
        *,
        ttl_seconds: int = DEFAULT_DRAFT_TTL_SECONDS,
        max_active_drafts: int = DEFAULT_MAX_ACTIVE_DRAFTS,
        clock: Callable[[], float] = time.monotonic,
        token_factory: Callable[[], str] = lambda: secrets.token_urlsafe(32),
    ) -> None:
        if isinstance(ttl_seconds, bool) or not isinstance(ttl_seconds, int) or not 1 <= ttl_seconds <= 300:
            raise ValueError("draft TTL is outside the supported range")
        if (
            isinstance(max_active_drafts, bool)
            or not isinstance(max_active_drafts, int)
            or not 1 <= max_active_drafts <= 1_024
        ):
            raise ValueError("draft capacity is outside the supported range")
        self._ttl_seconds = ttl_seconds
        self._max_active_drafts = max_active_drafts
        self._clock = clock
        self._token_factory = token_factory
        self._drafts: dict[str, Draft] = {}
        self._lock = threading.Lock()

    @property
    def ttl_seconds(self) -> int:
        return self._ttl_seconds

    def create(self, *, room_id: int, target_identity: str, text: str) -> Draft:
        now = self._clock()
        with self._lock:
            self._purge_expired(now)
            if len(self._drafts) >= self._max_active_drafts:
                raise DraftError("unavailable")
            for _attempt in range(4):
                token = self._token_factory()
                if (
                    isinstance(token, str)
                    and 16 <= len(token) <= MAX_DRAFT_ID_CHARS
                    and token.isascii()
                    and token not in self._drafts
                ):
                    draft = Draft(token, room_id, target_identity, text, now + self._ttl_seconds)
                    self._drafts[token] = draft
                    return draft
        raise DraftError("unavailable")

    def consume(self, draft_id: Any) -> Draft:
        if not isinstance(draft_id, str) or not 1 <= len(draft_id) <= MAX_DRAFT_ID_CHARS or not draft_id.isascii():
            raise DraftError("invalid_draft")
        now = self._clock()
        with self._lock:
            draft = self._drafts.pop(draft_id, None)
            if draft is None:
                self._purge_expired(now)
                raise DraftError("invalid_draft")
            if now >= draft.expires_at:
                raise DraftError("expired")
            return draft

    def _purge_expired(self, now: float) -> None:
        expired = [token for token, draft in self._drafts.items() if now >= draft.expires_at]
        for token in expired:
            del self._drafts[token]


def _validate_target_identity(value: Any) -> str:
    if not isinstance(value, str):
        raise OutboundError("invalid_request")
    if unicodedata.normalize("NFC", value) != value or not value or value != value.strip():
        raise OutboundError("invalid_request")
    if len(value) > MAX_ROOM_IDENTITY_CHARS or len(value.encode("utf-8")) > MAX_ROOM_IDENTITY_BYTES:
        raise OutboundError("invalid_request")
    if any(unicodedata.category(character).startswith("C") for character in value):
        raise OutboundError("invalid_request")
    return value


def _validate_text(value: Any) -> str:
    if not isinstance(value, str) or not value:
        raise OutboundError("invalid_request")
    if len(value) > MAX_TEXT_CHARS or len(value.encode("utf-8")) > MAX_TEXT_BYTES:
        raise OutboundError("invalid_request")
    if any(
        unicodedata.category(character).startswith("C") and character not in {"\n", "\t"}
        for character in value
    ):
        raise OutboundError("invalid_request")
    return value


def _validate_room_id(value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 9_223_372_036_854_775_807:
        raise OutboundError("invalid_request")
    return value


def _room_matches(service: Any, *, room_id: int, target_identity: str) -> bool:
    room = service.resolve_room_identity(
        room_id=room_id,
        display_identity=target_identity,
    )
    return (
        isinstance(room, Mapping)
        and type(room.get("chatId")) is int
        and room.get("chatId") == room_id
        and room.get("displayName") == target_identity
    )


class OutboundCoordinator:
    """Enforce all non-AX gates before invoking an injected adapter."""

    def __init__(self, *, service: Any, adapter: AXAdapter, drafts: DraftStore, enabled: bool) -> None:
        self._service = service
        self._adapter = adapter
        self._drafts = drafts
        self._enabled = enabled
        self._send_lock = threading.Lock()

    def preview(self, *, room_id: Any, target_identity: Any, text: Any) -> Preview:
        validated_room_id = _validate_room_id(room_id)
        validated_identity = _validate_target_identity(target_identity)
        validated_text = _validate_text(text)
        try:
            matches = _room_matches(
                self._service,
                room_id=validated_room_id,
                target_identity=validated_identity,
            )
        except OutboundError:
            raise
        except Exception as exc:
            raise OutboundError("source_unavailable") from exc
        if not matches:
            raise OutboundError("target_invalid")
        draft = self._drafts.create(
            room_id=validated_room_id,
            target_identity=validated_identity,
            text=validated_text,
        )
        return Preview(
            draft_id=draft.draft_id,
            target_identity=draft.target_identity,
            text=draft.text,
            expires_in_seconds=self._drafts.ttl_seconds,
        )

    def confirm(self, *, draft_id: Any, confirmation: Any) -> OutboundResult:
        if confirmation != CONFIRMATION_VALUE:
            raise OutboundError("confirmation_required")
        draft = self._drafts.consume(draft_id)
        if not self._enabled:
            raise OutboundError("disabled")
        try:
            matches = _room_matches(
                self._service,
                room_id=draft.room_id,
                target_identity=draft.target_identity,
            )
        except OutboundError:
            raise
        except Exception as exc:
            raise OutboundError("source_unavailable") from exc
        if not matches:
            raise OutboundError("target_invalid")
        with self._send_lock:
            if not self._adapter.check_availability().is_available:
                raise OutboundError("unavailable")
            preparation = self._adapter.prepare_text(
                expected_identity=draft.target_identity,
                text=draft.text,
            )
            if preparation.status in {"mismatch", "readback_mismatch"}:
                raise OutboundError("mismatch")
            if preparation.status != "ready" or not preparation.receipt:
                raise OutboundError("unavailable")
            outcome = self._adapter.send_prepared(preparation)
            if outcome.status == "mismatch":
                raise OutboundError("mismatch")
            if outcome.status != "invoked":
                raise OutboundError("unavailable")
        return OutboundResult(
            status="uncertain",
            room_id=draft.room_id,
            verification_path=f"/api/v1/rooms/{draft.room_id}/messages?limit=50",
        )


__all__ = [
    "CONFIRMATION_VALUE",
    "AXAdapter",
    "AXAvailability",
    "AXPreparation",
    "AXSendOutcome",
    "DraftError",
    "DraftStore",
    "OutboundCoordinator",
    "OutboundError",
    "OutboundResult",
    "Preview",
]
