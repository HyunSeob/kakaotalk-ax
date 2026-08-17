"""Production launcher for the loopback-only web viewer."""

from __future__ import annotations

from .app import create_app
from .config import Settings


def run_server(settings: Settings) -> None:
    """Start uvicorn only after secure settings have been loaded."""

    import uvicorn

    application = create_app(settings=settings)
    uvicorn.run(
        application,
        host=settings.host,
        port=settings.port,
        access_log=False,
        proxy_headers=False,
        server_header=False,
    )


__all__ = ["run_server"]
