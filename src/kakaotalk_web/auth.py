"""Identity gate for requests arriving through Tailscale Serve."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import Any

from starlette.datastructures import Headers
from starlette.responses import JSONResponse

from .config import ConfigError, Settings, normalize_login


IDENTITY_HEADER = "tailscale-user-login"


def is_protected_path(path: str) -> bool:
    return (
        path == "/"
        or path == "/assets"
        or path.startswith("/assets/")
        or path == "/api/v1"
        or path.startswith("/api/v1/")
    )


def _denied(status_code: int) -> JSONResponse:
    if status_code == 401:
        body = {"error": {"code": "identity_required", "message": "Identity required."}}
    else:
        body = {"error": {"code": "identity_forbidden", "message": "Identity not allowed."}}
    return JSONResponse(body, status_code=status_code)


class IdentityMiddleware:
    """Require one normalized, exact Tailscale identity on protected paths."""

    def __init__(self, app: Callable[..., Awaitable[Any]], *, settings: Settings) -> None:
        self.app = app
        self.settings = settings

    async def __call__(self, scope: dict[str, Any], receive: Any, send: Any) -> None:
        if scope.get("type") != "http" or not is_protected_path(str(scope.get("path", ""))):
            await self.app(scope, receive, send)
            return

        if self.settings.auth_mode == "network":
            await self.app(scope, receive, send)
            return

        headers = Headers(scope=scope)
        identities = headers.getlist(IDENTITY_HEADER)
        if len(identities) != 1:
            response = _denied(401)
            await response(scope, receive, send)
            return
        if "," in identities[0]:
            response = _denied(401)
            await response(scope, receive, send)
            return
        try:
            identity = normalize_login(identities[0])
        except ConfigError:
            response = _denied(401)
            await response(scope, receive, send)
            return
        if identity not in self.settings.allowed_logins:
            response = _denied(403)
            await response(scope, receive, send)
            return
        await self.app(scope, receive, send)
