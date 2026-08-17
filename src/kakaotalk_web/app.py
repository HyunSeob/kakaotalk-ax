"""Small, read-only Starlette surface for the local KakaoTalk service."""

from __future__ import annotations

import json
import re
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Callable

from starlette.concurrency import run_in_threadpool
from starlette.middleware import Middleware
from starlette.requests import Request
from starlette.responses import FileResponse, JSONResponse
from starlette.routing import Mount, Route
from starlette.staticfiles import StaticFiles
from starlette.applications import Starlette
from starlette.types import Receive, Scope, Send

from kakaotalk_service import (
    DEFAULT_MESSAGE_LIMIT,
    DEFAULT_ROOM_LIMIT,
    MAX_KEYWORD_CHARS,
    MAX_MESSAGE_LIMIT,
    MAX_ROOM_LIMIT,
    KakaoTalkService,
    ServiceError,
    ValidationError,
    validate_id,
    validate_keyword,
    validate_limit,
)

from .auth import IdentityMiddleware
from .config import Settings, load_settings


STATIC_DIR = Path(__file__).with_name("static")
INDEX_PATH = STATIC_DIR / "index.html"
MAX_SEARCH_BODY_BYTES = 16_384
MAX_DISPLAY_NAME_BYTES = 512
MAX_TYPE_LABEL_BYTES = 64
INTEGER_MAX = 9_223_372_036_854_775_807
INTEGER_MIN = -INTEGER_MAX - 1
QUERY_INTEGER_RE = re.compile(r"[0-9]+\Z")

STATUS_FIELDS = (
    "databaseAvailable",
    "walPresent",
    "walChanged",
    "databaseChanged",
    "changeKnown",
)
class SecurityHeadersMiddleware:
    """Apply the same private-response policy to API, auth, and static output."""

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        async def send_with_headers(message: dict[str, Any]) -> None:
            if message.get("type") == "http.response.start":
                headers = list(message.get("headers", []))
                protected = {
                    b"cache-control": b"no-store",
                    b"content-security-policy": (
                        b"default-src 'self'; script-src 'self'; style-src 'self'; "
                        b"connect-src 'self'; object-src 'none'; base-uri 'none'; "
                        b"frame-ancestors 'none'; form-action 'self'"
                    ),
                    b"referrer-policy": b"no-referrer",
                    b"x-content-type-options": b"nosniff",
                    b"x-frame-options": b"DENY",
                    b"permissions-policy": b"camera=(), microphone=(), geolocation=()",
                }
                present = {key.lower() for key, _ in headers}
                headers.extend((key, value) for key, value in protected.items() if key not in present)
                message = dict(message)
                message["headers"] = headers
            await send(message)

        await self.app(scope, receive, send_with_headers)

    def __init__(self, app: Any) -> None:
        self.app = app


def _error(code: str, status_code: int, message: str) -> JSONResponse:
    return JSONResponse(
        {"error": {"code": code, "message": message}},
        status_code=status_code,
    )


def _invalid_request() -> JSONResponse:
    return _error("invalid_request", 400, "Invalid request.")


def _source_unavailable() -> JSONResponse:
    return _error("source_unavailable", 503, "Source unavailable.")


def _internal_error() -> JSONResponse:
    return _error("internal_error", 500, "Request failed.")


async def _service_response(
    request: Request,
    operation: Callable[[Any], Any],
    project: Callable[[Any], Any],
) -> JSONResponse:
    service = request.app.state.service
    try:
        result = await run_in_threadpool(operation, service)
        return JSONResponse(project(result))
    except ValidationError:
        return _invalid_request()
    except ServiceError:
        return _source_unavailable()
    except Exception:
        return _internal_error()


def _bounded_string(value: Any, *, fallback: str, max_bytes: int) -> str:
    if not isinstance(value, str):
        return fallback
    encoded = value.encode("utf-8")
    if len(encoded) <= max_bytes:
        return value
    return encoded[: max_bytes - 3].decode("utf-8", errors="ignore") + "..."


def _optional_integer(value: Any, *, minimum: int = INTEGER_MIN) -> int | None:
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    if value < minimum or value > INTEGER_MAX:
        return None
    return value


def _positive_integer(value: Any) -> int | None:
    normalized = _optional_integer(value, minimum=1)
    return normalized if normalized is not None else None


def _project_status(value: Any) -> dict[str, bool]:
    if not isinstance(value, Mapping):
        raise ValueError("status is not an object")
    projected: dict[str, bool] = {}
    for field in STATUS_FIELDS:
        field_value = value.get(field)
        if type(field_value) is not bool:
            raise ValueError("status field is not boolean")
        projected[field] = field_value
    return projected


def _project_rooms(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        raise ValueError("rooms is not a list")
    projected: list[dict[str, Any]] = []
    for row in value:
        if not isinstance(row, Mapping):
            raise ValueError("room is not an object")
        chat_id = _positive_integer(row.get("chatId"))
        if chat_id is None:
            continue
        last_log_id = _optional_integer(row.get("lastLogId"), minimum=0)
        member_count = _optional_integer(row.get("activeMembersCount"), minimum=0)
        projected.append(
            {
                "chatId": chat_id,
                "displayName": _bounded_string(
                    row.get("displayName"), fallback="(unnamed)", max_bytes=MAX_DISPLAY_NAME_BYTES
                ),
                "roomType": _bounded_string(
                    row.get("roomType"), fallback="unknown", max_bytes=MAX_TYPE_LABEL_BYTES
                ),
                "lastLogId": last_log_id,
                "activeMembersCount": member_count,
            }
        )
    return projected


def _project_messages(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        raise ValueError("messages is not a list")
    projected: list[dict[str, Any]] = []
    for row in value:
        if not isinstance(row, Mapping):
            raise ValueError("message is not an object")
        attachment = row.get("attachment")
        projected.append(
            {
                "chatId": _positive_integer(row.get("chatId")),
                "roomName": _bounded_string(
                    row.get("roomName"), fallback="(unnamed)", max_bytes=MAX_DISPLAY_NAME_BYTES
                ),
                "logId": _optional_integer(row.get("logId"), minimum=0),
                "nickName": _bounded_string(
                    row.get("nickName"), fallback="", max_bytes=MAX_DISPLAY_NAME_BYTES
                ),
                "type": _optional_integer(row.get("type")),
                "typeLabel": _bounded_string(
                    row.get("typeLabel"), fallback="unknown", max_bytes=MAX_TYPE_LABEL_BYTES
                ),
                "sentAt": _optional_integer(row.get("sentAt")),
                "time": _bounded_string(row.get("time"), fallback="", max_bytes=64),
                "message": _bounded_string(row.get("message"), fallback="", max_bytes=8_192),
                "hasAttachment": isinstance(attachment, str) and bool(attachment),
            }
        )
    return projected


def _single_query_value(request: Request, name: str) -> str | None:
    values = request.query_params.getlist(name)
    if len(values) > 1:
        raise ValidationError("query parameter is repeated")
    return values[0] if values else None


def _query_limit(request: Request, *, default: int, maximum: int) -> int:
    value = _single_query_value(request, "limit")
    if value is None or not QUERY_INTEGER_RE.fullmatch(value):
        if value is None:
            return validate_limit(None, default=default, maximum=maximum)
        raise ValidationError("limit must be an integer")
    return validate_limit(int(value), default=default, maximum=maximum)


def _room_id(path_value: Any) -> int:
    if not isinstance(path_value, str) or not QUERY_INTEGER_RE.fullmatch(path_value):
        raise ValidationError("room_id must be a positive integer")
    validated = validate_id(int(path_value), "room_id")
    if validated is None:
        raise ValidationError("room_id must be a positive integer")
    return validated


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


async def _search_payload(request: Request) -> tuple[str, int]:
    content_type = request.headers.get("content-type", "").split(";", 1)[0].strip().lower()
    if content_type != "application/json":
        raise ValidationError("search requires JSON")
    body = await request.body()
    if len(body) > MAX_SEARCH_BODY_BYTES:
        raise ValidationError("search body is too large")
    try:
        payload = json.loads(
            body.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise ValidationError("search body is invalid") from exc
    if not isinstance(payload, dict) or set(payload) - {"keyword", "limit"}:
        raise ValidationError("search body fields are invalid")
    keyword = validate_keyword(payload.get("keyword"))
    raw_limit = payload.get("limit", DEFAULT_MESSAGE_LIMIT)
    limit = validate_limit(raw_limit, default=DEFAULT_MESSAGE_LIMIT, maximum=MAX_MESSAGE_LIMIT)
    if len(keyword) > MAX_KEYWORD_CHARS:
        raise ValidationError("keyword is too long")
    return keyword, limit


async def healthz(_request: Request) -> JSONResponse:
    return JSONResponse({"ok": True})


async def index(_request: Request) -> FileResponse:
    return FileResponse(INDEX_PATH, media_type="text/html; charset=utf-8")


async def status(request: Request) -> JSONResponse:
    return await _service_response(request, lambda service: service.status(), _project_status)


async def rooms(request: Request) -> JSONResponse:
    try:
        limit = _query_limit(request, default=DEFAULT_ROOM_LIMIT, maximum=MAX_ROOM_LIMIT)
    except ValidationError:
        return _invalid_request()
    return await _service_response(
        request,
        lambda service: service.list_rooms(limit=limit),
        _project_rooms,
    )


async def messages(request: Request) -> JSONResponse:
    try:
        room_id = _room_id(request.path_params.get("room_id"))
        limit = _query_limit(request, default=DEFAULT_MESSAGE_LIMIT, maximum=MAX_MESSAGE_LIMIT)
    except ValidationError:
        return _invalid_request()
    return await _service_response(
        request,
        lambda service: service.read_messages(room_id=room_id, limit=limit),
        _project_messages,
    )


async def search(request: Request) -> JSONResponse:
    try:
        keyword, limit = await _search_payload(request)
    except ValidationError:
        return _invalid_request()
    return await _service_response(
        request,
        lambda service: service.search_messages(keyword=keyword, limit=limit),
        _project_messages,
    )


def create_app(
    *,
    service: Any | None = None,
    settings: Settings | None = None,
) -> Starlette:
    """Build the app with an injectable service; production loads secure config."""

    resolved_settings = settings if settings is not None else load_settings()
    if not INDEX_PATH.is_file() or not (STATIC_DIR / "app.js").is_file() or not (STATIC_DIR / "style.css").is_file():
        raise RuntimeError("web assets are unavailable")
    resolved_service = service if service is not None else KakaoTalkService()
    routes = [
        Route("/healthz", healthz, methods=["GET"]),
        Route("/", index, methods=["GET"]),
        Route("/api/v1/status", status, methods=["GET"]),
        Route("/api/v1/rooms", rooms, methods=["GET"]),
        Route("/api/v1/rooms/{room_id}/messages", messages, methods=["GET"]),
        Route("/api/v1/search", search, methods=["POST"]),
        Mount("/assets", app=StaticFiles(directory=STATIC_DIR, check_dir=True)),
    ]
    app = Starlette(
        debug=False,
        routes=routes,
        middleware=[
            Middleware(SecurityHeadersMiddleware),
            Middleware(IdentityMiddleware, settings=resolved_settings),
        ],
    )
    app.state.service = resolved_service
    app.state.settings = resolved_settings
    return app


__all__ = ["create_app"]
