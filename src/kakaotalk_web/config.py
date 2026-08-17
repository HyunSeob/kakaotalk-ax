"""Fail-closed local configuration for the private web viewer."""

from __future__ import annotations

import json
import os
import stat
import tempfile
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


LOOPBACK_HOST = "127.0.0.1"
DEFAULT_CONFIG_PATH = Path.home() / ".config" / "kakaotalk-reader" / "web.json"
DEFAULT_PORT = 8765
DEFAULT_REFRESH_SECONDS = 15
MAX_REFRESH_SECONDS = 3_600
MAX_LOGIN_LENGTH = 320
MAX_CONFIG_BYTES = 65_536
AUTH_MODE_IDENTITY = "identity"
AUTH_MODE_NETWORK = "network"
AUTH_MODES = frozenset({AUTH_MODE_IDENTITY, AUTH_MODE_NETWORK})


class ConfigError(ValueError):
    """The local web configuration is missing or unsafe to use."""


def normalize_login(value: Any) -> str:
    """Normalize one identity without applying aliases or broad matching."""

    if not isinstance(value, str):
        raise ConfigError("login must be a string")
    normalized = unicodedata.normalize("NFKC", value).strip().casefold()
    if not normalized or len(normalized) > MAX_LOGIN_LENGTH:
        raise ConfigError("login is empty or too long")
    if any(unicodedata.category(character).startswith("C") for character in normalized):
        raise ConfigError("login contains a control character")
    return normalized


def _validated_port(value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 65_535:
        raise ConfigError("port is outside the supported range")
    return value


def _validated_refresh_seconds(value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= MAX_REFRESH_SECONDS:
        raise ConfigError("refreshSeconds is outside the supported range")
    return value


def _validated_logins(values: Any) -> tuple[str, ...]:
    if not isinstance(values, list) or not values:
        raise ConfigError("allowedLogins must be a non-empty list")
    normalized = tuple(normalize_login(value) for value in values)
    if len(set(normalized)) != len(normalized):
        raise ConfigError("allowedLogins contains duplicates")
    return normalized


def _validated_auth_mode(value: Any) -> str:
    if not isinstance(value, str) or value not in AUTH_MODES:
        raise ConfigError("authMode is unsupported")
    return value


@dataclass(frozen=True)
class OutboundSettings:
    """Explicit, fail-closed gate for the separate outbound adapter."""

    enabled: bool = False
    helper_path: Path | None = None

    def __post_init__(self) -> None:
        if type(self.enabled) is not bool:
            raise ConfigError("outbound enabled flag is invalid")
        if not self.enabled:
            if self.helper_path is not None:
                raise ConfigError("disabled outbound cannot configure a helper")
            return
        if not isinstance(self.helper_path, Path) or not self.helper_path.is_absolute():
            raise ConfigError("enabled outbound requires an absolute helper path")


def _validated_outbound(value: Any) -> OutboundSettings:
    if value is None:
        return OutboundSettings()
    if not isinstance(value, dict) or set(value) - {"enabled", "helperPath"}:
        raise ConfigError("outbound config is invalid")
    enabled = value.get("enabled", False)
    if type(enabled) is not bool:
        raise ConfigError("outbound enabled flag is invalid")
    helper_value = value.get("helperPath")
    if helper_value is not None and not isinstance(helper_value, str):
        raise ConfigError("outbound helper path is invalid")
    helper_path = Path(helper_value) if helper_value is not None else None
    return OutboundSettings(enabled=enabled, helper_path=helper_path)


@dataclass(frozen=True)
class Settings:
    """Validated runtime settings; the bind host is intentionally fixed."""

    allowed_logins: tuple[str, ...] = ()
    port: int = DEFAULT_PORT
    refresh_seconds: int = DEFAULT_REFRESH_SECONDS
    host: str = LOOPBACK_HOST
    auth_mode: str = AUTH_MODE_IDENTITY
    outbound: OutboundSettings = OutboundSettings()

    def __post_init__(self) -> None:
        auth_mode = _validated_auth_mode(self.auth_mode)
        if auth_mode == AUTH_MODE_IDENTITY:
            if not isinstance(self.allowed_logins, (list, tuple)):
                raise ConfigError("allowedLogins must be a non-empty list")
            allowed_logins = _validated_logins(list(self.allowed_logins))
        else:
            if not isinstance(self.allowed_logins, (list, tuple)):
                raise ConfigError("allowedLogins must be a list")
            if self.allowed_logins:
                raise ConfigError("network auth mode cannot contain an identity allowlist")
            allowed_logins = ()
        object.__setattr__(self, "auth_mode", auth_mode)
        object.__setattr__(self, "allowed_logins", allowed_logins)
        object.__setattr__(self, "port", _validated_port(self.port))
        object.__setattr__(self, "refresh_seconds", _validated_refresh_seconds(self.refresh_seconds))
        if self.host != LOOPBACK_HOST:
            raise ConfigError("web host is fixed to loopback")
        if not isinstance(self.outbound, OutboundSettings):
            raise ConfigError("outbound settings are invalid")


def _check_secure_file(path: Path) -> os.stat_result:
    try:
        metadata = path.lstat()
    except (FileNotFoundError, OSError) as exc:
        raise ConfigError("web config is unavailable") from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise ConfigError("web config must be a regular file")
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise ConfigError("web config owner is invalid")
    if stat.S_IMODE(metadata.st_mode) != 0o600:
        raise ConfigError("web config permissions are invalid")
    return metadata


def load_settings(path: str | os.PathLike[str] | None = None) -> Settings:
    """Load and validate the required mode-0600 production config."""

    config_path = Path(path) if path is not None else DEFAULT_CONFIG_PATH
    metadata = _check_secure_file(config_path)
    if metadata.st_size > MAX_CONFIG_BYTES:
        raise ConfigError("web config is too large")
    try:
        with config_path.open("rb") as stream:
            raw = stream.read(MAX_CONFIG_BYTES + 1)
        if len(raw) > MAX_CONFIG_BYTES:
            raise ConfigError("web config is too large")
        payload = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_json_keys,
        )
    except ConfigError:
        raise
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as exc:
        raise ConfigError("web config is invalid") from exc
    if not isinstance(payload, dict):
        raise ConfigError("web config must be an object")
    if set(payload) - {"allowedLogins", "authMode", "port", "refreshSeconds", "outbound"}:
        raise ConfigError("web config contains unsupported fields")
    auth_mode = payload.get("authMode", AUTH_MODE_IDENTITY)
    try:
        return Settings(
            allowed_logins=payload.get("allowedLogins", []),
            port=payload.get("port", DEFAULT_PORT),
            refresh_seconds=payload.get("refreshSeconds", DEFAULT_REFRESH_SECONDS),
            auth_mode=auth_mode,
            outbound=_validated_outbound(payload.get("outbound")),
        )
    except (TypeError, ValueError) as exc:
        raise ConfigError("web config is invalid") from exc


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate web config key")
        result[key] = value
    return result


def write_config(
    path: str | os.PathLike[str] | None,
    *,
    allowed_logins: Iterable[Any],
    port: int = DEFAULT_PORT,
    refresh_seconds: int = DEFAULT_REFRESH_SECONDS,
    auth_mode: str = AUTH_MODE_IDENTITY,
) -> Settings:
    """Atomically write a local mode-0600 config and return its settings."""

    values = list(allowed_logins)
    settings = Settings(
        allowed_logins=tuple(values),
        port=port,
        refresh_seconds=refresh_seconds,
        auth_mode=auth_mode,
    )
    config_path = Path(path) if path is not None else DEFAULT_CONFIG_PATH
    parent = config_path.parent
    parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        os.chmod(parent, 0o700)
    except OSError as exc:
        raise ConfigError("web config directory is unavailable") from exc

    payload = {
        "allowedLogins": list(settings.allowed_logins),
        "authMode": settings.auth_mode,
        "port": settings.port,
        "refreshSeconds": settings.refresh_seconds,
    }
    temporary_path: str | None = None
    try:
        descriptor, temporary_path = tempfile.mkstemp(
            prefix=f".{config_path.name}.",
            suffix=".tmp",
            dir=parent,
            text=True,
        )
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(payload, stream, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, config_path)
        temporary_path = None
        os.chmod(config_path, 0o600)
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
    except (OSError, TypeError, ValueError) as exc:
        raise ConfigError("web config could not be written") from exc
    finally:
        if temporary_path is not None:
            try:
                os.unlink(temporary_path)
            except FileNotFoundError:
                pass
    return settings
