"""Private, atomic send journals and process locks (no app data access)."""

import errno
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile
from contextlib import contextmanager

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes
    import msvcrt
else:
    import fcntl


def state_directory():
    if sys.platform == "win32":
        return Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData/Local") / "kakaotalk-ax"
    return Path.home() / ".config/kakaotalk-reader"


def digest(value):
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def open_private(path, writable=False):
    """Open a regular state file without following a final link or reparse point."""
    if sys.platform != "win32":
        flags = (os.O_CREAT | os.O_RDWR) if writable else os.O_RDONLY
        return os.open(path, flags | os.O_NOFOLLOW, 0o600)

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    create = kernel.CreateFileW
    create.argtypes = (wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                       wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE)
    create.restype = wintypes.HANDLE
    close = kernel.CloseHandle
    close.argtypes = (wintypes.HANDLE,)
    close.restype = wintypes.BOOL
    info = kernel.GetFileInformationByHandleEx
    info.argtypes = (wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD)
    info.restype = wintypes.BOOL
    access = 0xc0000000 if writable else 0x80000000  # GENERIC_READ | GENERIC_WRITE
    # Share read/write/delete and open the reparse point itself for inspection.
    handle = create(str(path), access, 7, None, 4 if writable else 3, 0x00200000, None)
    if handle == ctypes.c_void_p(-1).value:
        error = ctypes.get_last_error()
        if error in (2, 3):
            raise FileNotFoundError(errno.ENOENT, str(path))
        raise ctypes.WinError(error)
    try:
        attributes = (wintypes.DWORD * 2)()
        if not info(handle, 9, ctypes.byref(attributes), ctypes.sizeof(attributes)):
            raise ctypes.WinError(ctypes.get_last_error())
        if attributes[0] & (0x400 | 0x10):  # REPARSE_POINT | DIRECTORY
            raise OSError(errno.ELOOP, "Unsafe state file", str(path))
        return msvcrt.open_osfhandle(handle, os.O_BINARY | (os.O_RDWR if writable else os.O_RDONLY))
    except BaseException:
        close(handle)
        raise


def replace_private(temporary, path):
    if sys.platform == "win32":
        move = ctypes.WinDLL("kernel32", use_last_error=True).MoveFileExW
        move.argtypes = (wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD)
        move.restype = wintypes.BOOL
        if not move(temporary, str(path), 0x9):  # REPLACE_EXISTING | WRITE_THROUGH
            raise ctypes.WinError(ctypes.get_last_error())
        return
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def save_private(path, value):
    descriptor, temporary = tempfile.mkstemp(prefix=".operation-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump(value, stream, ensure_ascii=False)
            stream.flush()
            os.fsync(stream.fileno())
        replace_private(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


@contextmanager
def process_lock(root, busy_message):
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor = open_private(root / ".lock", writable=True)
    try:
        try:
            if sys.platform == "win32":
                msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
            else:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as error:
            if isinstance(error, BlockingIOError) or (sys.platform == "win32" and error.errno == errno.EACCES):
                raise ValueError(busy_message) from None
            raise
        yield
    finally:
        os.close(descriptor)


def send_lock(root):
    return process_lock(root, "Another KakaoTalk send is in progress")
