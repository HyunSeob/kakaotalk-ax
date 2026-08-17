"""Local subprocess boundary for the rebuildable macOS AX helper."""

from __future__ import annotations

import json
import os
import secrets
import stat
import subprocess
import threading
from pathlib import Path
from typing import Callable

from . import AXAvailability, AXPreparation, AXSendOutcome


MAX_HELPER_OUTPUT_BYTES = 256
MAX_PENDING_PREPARATIONS = 32


def _secure_helper_path(path: Path) -> bool:
    """Revalidate the executable and non-writable ownership chain per call."""

    try:
        metadata = path.lstat()
        uid = os.getuid() if hasattr(os, "getuid") else metadata.st_uid
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_uid != uid
            or metadata.st_mode & 0o077
            or not metadata.st_mode & stat.S_IXUSR
        ):
            return False
        current = Path(path.anchor)
        for component in path.parts[1:-1]:
            current /= component
            ancestor = current.lstat()
            if stat.S_ISLNK(ancestor.st_mode):
                return False
            if (
                not stat.S_ISDIR(ancestor.st_mode)
                or ancestor.st_uid not in {0, uid}
                or ancestor.st_mode & 0o022
            ):
                return False
    except (OSError, ValueError):
        return False
    return True


class HelperAXAdapter:
    """Invoke an absolute local helper without placing private payloads in argv."""

    def __init__(
        self,
        helper_path: Path,
        *,
        timeout_seconds: int = 5,
        path_validator: Callable[[Path], bool] = _secure_helper_path,
    ) -> None:
        if not isinstance(helper_path, Path) or not helper_path.is_absolute():
            raise ValueError("helper path must be absolute")
        if isinstance(timeout_seconds, bool) or not isinstance(timeout_seconds, int) or not 1 <= timeout_seconds <= 15:
            raise ValueError("helper timeout is invalid")
        self._helper_path = str(helper_path)
        self._timeout_seconds = timeout_seconds
        self._path_validator = path_validator
        self._pending: dict[str, tuple[str, str]] = {}
        self._lock = threading.Lock()

    def _invoke(self, operation: str, payload: dict[str, str] | None = None) -> str | None:
        helper_path = Path(self._helper_path)
        if not self._path_validator(helper_path):
            return None
        encoded_input = "" if payload is None else json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        try:
            completed = subprocess.run(
                [self._helper_path, operation],
                input=encoded_input,
                text=True,
                capture_output=True,
                timeout=self._timeout_seconds,
                check=False,
                close_fds=True,
                env={"PATH": "/usr/bin:/bin", "LANG": "en_US.UTF-8"},
            )
        except (OSError, subprocess.SubprocessError):
            return None
        if completed.returncode != 0 or len(completed.stdout.encode("utf-8")) > MAX_HELPER_OUTPUT_BYTES:
            return None
        try:
            response = json.loads(completed.stdout)
        except (UnicodeError, json.JSONDecodeError):
            return None
        if not isinstance(response, dict) or set(response) != {"status"} or not isinstance(response["status"], str):
            return None
        return response["status"]

    def check_availability(self) -> AXAvailability:
        return AXAvailability(self._invoke("availability") == "available")

    def prepare_text(self, *, expected_identity: str, text: str) -> AXPreparation:
        status = self._invoke("prepare", {"targetIdentity": expected_identity, "text": text})
        if status != "ready":
            if status == "mismatch":
                return AXPreparation.mismatch()
            if status == "readback_mismatch":
                return AXPreparation.readback_mismatch()
            return AXPreparation.unavailable()
        with self._lock:
            if len(self._pending) >= MAX_PENDING_PREPARATIONS:
                return AXPreparation.unavailable()
            receipt = secrets.token_urlsafe(24)
            self._pending[receipt] = (expected_identity, text)
        return AXPreparation.ready(receipt)

    def send_prepared(self, preparation: AXPreparation) -> AXSendOutcome:
        receipt = preparation.receipt
        if preparation.status != "ready" or not isinstance(receipt, str):
            raise ValueError("preparation is not sendable")
        with self._lock:
            payload = self._pending.pop(receipt, None)
        if payload is None:
            raise ValueError("preparation is unavailable")
        status = self._invoke("send", {"targetIdentity": payload[0], "text": payload[1]})
        if status == "invoked":
            return AXSendOutcome.invoked()
        if status == "mismatch":
            return AXSendOutcome.mismatch()
        return AXSendOutcome.unavailable()


__all__ = ["HelperAXAdapter"]
