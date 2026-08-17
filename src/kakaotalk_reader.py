#!/usr/bin/env python3
"""
KakaoTalk Mac DB Reader - CLI tool to read KakaoTalk Mac's local encrypted database.

Requires: sqlcipher (brew install sqlcipher)
No Python packages needed beyond stdlib.
"""

import argparse
import base64
import hashlib
import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone, timedelta
from multiprocessing import Pool, cpu_count
from typing import Any, Mapping

# ─── Constants ───────────────────────────────────────────────────────────────

KAKAO_BASE_PATH = os.path.expanduser(
    "~/Library/Containers/com.kakao.KakaoTalkMac/Data/Library/Application Support/com.kakao.KakaoTalkMac/"
)
KAKAO_PLIST_PATH = os.path.expanduser(
    "~/Library/Containers/com.kakao.KakaoTalkMac/Data/Library/Preferences/com.kakao.KakaoTalkMac.plist"
)
KAKAO_PLIST2_GLOB = os.path.expanduser(
    "~/Library/Containers/com.kakao.KakaoTalkMac/Data/Library/Preferences/com.kakao.KakaoTalkMac.*.plist"
)
CONFIG_PATH = os.path.expanduser(
    os.environ.get("KAKAOTALK_READER_CONFIG", "~/.config/kakaotalk-reader/config.json")
)
KST = timezone(timedelta(hours=9))
SQLCIPHER_BIN = shutil.which("sqlcipher") or "/opt/homebrew/opt/sqlcipher/bin/sqlcipher"
LIVE_READ_RETRIES = 3
LIVE_READ_BUSY_TIMEOUT_MS = 5_000
SQL_MAX_ROWS = 500


# ─── Crypto (from blluv's gist) ─────────────────────────────────────────────

def pbkdf2(password: bytes, salt: bytes) -> bytes:
    return hashlib.pbkdf2_hmac("sha256", password, salt, 100000, 128)


def hashed_device_uuid(uuid: str) -> str:
    b = uuid.encode("ascii")
    return base64.b64encode(hashlib.sha1(b).digest() + hashlib.sha256(b).digest()).decode("ascii")


def get_database_name(user_id: int, uuid: str) -> str:
    hawawa = ".".join([".", "F", str(user_id), "A", "F", "".join(reversed(uuid)), ".", "|"])
    salt = "".join(reversed(hashed_device_uuid(uuid))).encode("ascii")
    return pbkdf2(hawawa.encode("ascii"), salt).hex()[28:28 + 78]


def get_secure_key(user_id: int, uuid: str) -> str:
    h = hashed_device_uuid(uuid)
    hawawa = "F".join(["A", h, "|", "F", uuid[:5], "H", str(user_id), "|", uuid[7:]])
    return pbkdf2("".join(reversed(hawawa)).encode("ascii"), uuid[int(len(uuid) * 0.3):].encode("ascii")).hex()


# ─── System helpers ──────────────────────────────────────────────────────────

def get_hardware_uuid() -> str:
    try:
        r = subprocess.run(["ioreg", "-rd1", "-c", "IOPlatformExpertDevice"], capture_output=True, text=True, check=True)
        for line in r.stdout.splitlines():
            if "IOPlatformUUID" in line:
                return line.split('"')[-2]
    except Exception:
        pass
    return ""


def find_db_files() -> list[dict]:
    if not os.path.isdir(KAKAO_BASE_PATH):
        return []
    results = []
    for entry in os.scandir(KAKAO_BASE_PATH):
        if not entry.is_file() or entry.name.startswith("."):
            continue
        if entry.name.endswith(("-shm", "-wal")):
            continue
        st = entry.stat()
        if st.st_size < 10000:
            continue
        results.append({
            "path": entry.path, "name": entry.name,
            "size": st.st_size, "has_wal": os.path.exists(entry.path + "-wal"),
            "mtime": datetime.fromtimestamp(st.st_mtime).isoformat(),
        })
    results.sort(key=lambda x: (not x["has_wal"], -x["size"]))
    return results


def _file_signature(path: str) -> tuple[int, int] | None:
    try:
        if not os.path.isfile(path):
            return None
        stat_result = os.stat(path)
    except OSError:
        return None
    return int(stat_result.st_size), int(stat_result.st_mtime_ns)


def database_file_status(db_path: str) -> dict[str, Any]:
    """Inspect database/WAL presence without opening or modifying either file."""

    database_signature = _file_signature(db_path)
    wal_signature = _file_signature(db_path + "-wal")
    return {
        "available": database_signature is not None,
        "wal_present": wal_signature is not None,
        "database_signature": database_signature,
        "wal_signature": wal_signature,
    }


def extract_user_id_from_plist() -> int | None:
    """Extract userId from plist by analyzing FSChatWindowTransparency keys.

    These keys follow the pattern: FSChatWindowTransparency<chatId><userId>
    The userId is the common suffix across all such keys.
    """
    import glob
    plist_files = [KAKAO_PLIST_PATH] + glob.glob(KAKAO_PLIST2_GLOB)

    for pf in plist_files:
        if not os.path.exists(pf):
            continue
        try:
            with open(pf, "rb") as f:
                plist = plistlib.load(f)
        except Exception:
            continue

        # Look for FSChatWindowTransparency keys
        fs_keys = [k for k in plist if k.startswith("FSChatWindowTransparency")]
        if len(fs_keys) >= 2:
            # Extract numeric suffix. The userId is the common ending.
            suffixes = [k.replace("FSChatWindowTransparency", "") for k in fs_keys]
            # Find longest common suffix
            if suffixes:
                rev = [s[::-1] for s in suffixes]
                common = ""
                for chars in zip(*rev):
                    if len(set(chars)) == 1:
                        common += chars[0]
                    else:
                        break
                common = common[::-1]
                if common.isdigit() and len(common) >= 4:
                    return int(common)

        # Also check AlertKakaoIDsList - less reliable but worth trying
    return None


# ─── Parallel brute-force ────────────────────────────────────────────────────

def _check_range(args):
    start, end, uuid, target = args
    for uid in range(start, end):
        if get_database_name(uid, uuid) == target:
            return uid
    return None


def bruteforce_user_id(uuid: str, target: str, max_id: int = 2_000_000) -> int | None:
    cores = cpu_count() or 4
    chunk = max(1000, max_id // (cores * 4))
    ranges = [(s, min(s + chunk, max_id + 1), uuid, target) for s in range(1, max_id + 1, chunk)]
    print(f"Brute-forcing userId 1..{max_id} ({cores} cores)...", file=sys.stderr)

    with Pool(cores) as pool:
        for i, result in enumerate(pool.imap_unordered(_check_range, ranges)):
            if result is not None:
                pool.terminate()
                print("  Local KakaoTalk identity match found.", file=sys.stderr)
                return result
            if (i + 1) % (cores * 2) == 0:
                print(f"  ...checked ~{min((i + 1) * chunk, max_id):,}", file=sys.stderr)
    return None


# ─── SQLCipher ───────────────────────────────────────────────────────────────

def _parameter_command(name: str, value: Any) -> str:
    """Encode a bound value for sqlcipher's CLI parameter table.

    Strings are passed as UTF-8 blobs and cast back to text by the SQL
    statement.  This avoids putting user text into SQL source or relying on
    the CLI's shell-like quote parser for arbitrary Unicode and punctuation.
    """

    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
        raise ValueError("invalid SQL parameter name")
    if value is None:
        encoded = "NULL"
    elif isinstance(value, bool):
        encoded = "1" if value else "0"
    elif isinstance(value, int):
        encoded = str(value)
    elif isinstance(value, str):
        encoded = "X'" + value.encode("utf-8").hex() + "'"
    else:
        raise TypeError("SQL parameters must be strings, integers, booleans, or null")
    return f".parameter set :{name} {encoded}"


def _sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _safe_sqlcipher_error(returncode: int, stderr: str) -> str:
    lower = stderr.lower()
    if "file is not a database" in lower or "file is encrypted" in lower:
        return "decryption failed"
    if "busy" in lower or "locked" in lower:
        return "database is busy; KakaoTalk may be writing, please retry"
    if returncode == 124:
        return "database read timed out"
    return "sqlcipher read failed"


def _is_retryable_read_error(stderr: str) -> bool:
    lower = stderr.lower()
    return "busy" in lower or "locked" in lower or "database disk image is malformed" in lower


def sqlcipher_query(
    db_path: str,
    key: str,
    sql: str,
    json_mode: bool = True,
    *,
    params: Mapping[str, Any] | None = None,
    read_only: bool = True,
) -> str:
    """Run one query against a fresh, read-only live-DB snapshot.

    SQLite's WAL reader takes a snapshot when the transaction begins.  The
    explicit query-only transaction prevents accidental writes and the fresh
    process/retry handles a writer briefly holding the database.  The source
    database and its WAL are never copied, renamed, checkpointed, or modified.
    """

    if not os.path.isfile(db_path):
        raise RuntimeError("database file not found")
    if not os.path.isfile(SQLCIPHER_BIN):
        raise RuntimeError("sqlcipher is not installed; install it with Homebrew")
    if read_only:
        _assert_read_only_sql(sql)

    commands = [
        ".output /dev/null",
        "PRAGMA cipher_default_compatibility = 3;",
        f"PRAGMA KEY={_sql_literal(key)};",
        ".parameter init",
    ]
    if params:
        commands.extend(_parameter_command(name, params[name]) for name in sorted(params))
    if read_only:
        commands.extend([
            f"PRAGMA busy_timeout = {LIVE_READ_BUSY_TIMEOUT_MS};",
            "PRAGMA query_only = ON;",
            "BEGIN;",
        ])
    commands.extend([
        ".output stdout",
        ".mode json" if json_mode else ".mode column\n.headers on",
    ])
    commands.append(sql)
    if read_only:
        commands.append("COMMIT;")
    input_text = "\n".join(commands) + "\n"

    for attempt in range(LIVE_READ_RETRIES):
        try:
            result = subprocess.run(
                [SQLCIPHER_BIN, "-readonly", db_path],
                input=input_text,
                capture_output=True,
                text=True,
                timeout=30,
            )
        except subprocess.TimeoutExpired as exc:
            if attempt + 1 < LIVE_READ_RETRIES:
                time.sleep(0.1 * (attempt + 1))
                continue
            raise RuntimeError("database read timed out") from exc

        if result.returncode == 0:
            return result.stdout.strip()
        if read_only and attempt + 1 < LIVE_READ_RETRIES and _is_retryable_read_error(result.stderr):
            time.sleep(0.1 * (attempt + 1))
            continue
        raise RuntimeError(_safe_sqlcipher_error(result.returncode, result.stderr))

    raise RuntimeError("database read failed")


def query_json(
    db_path: str,
    key: str,
    sql: str,
    *,
    params: Mapping[str, Any] | None = None,
    read_only: bool = True,
    max_rows: int | None = None,
) -> list[dict]:
    raw = sqlcipher_query(db_path, key, sql, params=params, read_only=read_only)
    if not raw:
        return []
    if raw.startswith(("Error", "Parse error", "Runtime error")):
        raise RuntimeError("query returned an error")
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        idx = raw.find("[")
        if idx >= 0:
            try:
                parsed = json.loads(raw[idx:])
            except json.JSONDecodeError as nested_exc:
                raise RuntimeError("query returned invalid JSON") from nested_exc
        else:
            raise RuntimeError("query returned invalid JSON") from exc
    if not isinstance(parsed, list) or any(not isinstance(row, dict) for row in parsed):
        raise RuntimeError("query returned an unexpected result shape")
    if max_rows is not None:
        if isinstance(max_rows, bool) or not isinstance(max_rows, int) or max_rows < 1:
            raise ValueError("max_rows must be a positive integer")
        return parsed[:max_rows]
    return parsed


_DANGEROUS_SQL_TOKENS = {
    "ALTER", "ANALYZE", "ATTACH", "BEGIN", "COMMIT", "CREATE", "DELETE",
    "DETACH", "DROP", "END", "INSERT", "PRAGMA", "REINDEX", "REPLACE",
    "ROLLBACK", "SAVEPOINT", "UPDATE", "VACUUM",
}
_DANGEROUS_SQL_FUNCTIONS = {"LOAD_EXTENSION", "READFILE", "WRITEFILE", "SQLCIPHER_EXPORT"}


def _followed_by_open_paren(sql: str, index: int) -> bool:
    while index < len(sql) and sql[index].isspace():
        index += 1
    return index < len(sql) and sql[index] == "("


def _sql_tokens(sql: str) -> list[str]:
    if not isinstance(sql, str) or not sql.strip():
        raise ValueError("SQL query must not be empty")
    if "\x00" in sql:
        raise ValueError("SQL query contains an unsupported character")

    tokens: list[str] = []
    i = 0
    while i < len(sql):
        char = sql[i]
        if char in "'\"`":
            quote = char
            content_start = i + 1
            i += 1
            while i < len(sql):
                if sql[i] == quote:
                    if i + 1 < len(sql) and sql[i + 1] == quote and quote == "'":
                        i += 2
                        continue
                    content = sql[content_start:i]
                    i += 1
                    break
                if sql[i] == "\\" and quote in "\"`" and i + 1 < len(sql):
                    i += 2
                    continue
                i += 1
            else:
                raise ValueError("SQL query contains an unterminated quoted value")
            if (
                re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", content)
                and _followed_by_open_paren(sql, i)
            ):
                tokens.append(content.upper())
            continue
        if char == "[":
            end = sql.find("]", i + 1)
            if end < 0:
                raise ValueError("SQL query contains an unterminated identifier")
            content = sql[i + 1 : end]
            i = end + 1
            if (
                re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", content)
                and _followed_by_open_paren(sql, i)
            ):
                tokens.append(content.upper())
            continue
        if sql.startswith("--", i) or sql.startswith("/*", i):
            raise ValueError("SQL comments are not allowed")
        if char == ";":
            if sql[i + 1 :].strip():
                raise ValueError("only one SQL statement is allowed")
            i += 1
            continue
        match = re.match(r"[A-Za-z_][A-Za-z0-9_]*", sql[i:])
        if match:
            tokens.append(match.group(0).upper())
            i += len(match.group(0))
            continue
        i += 1
    return tokens


def _assert_read_only_sql(sql: str) -> None:
    """Keep even internal read calls from turning query_only off or writing."""

    tokens = _sql_tokens(sql)
    if not tokens or tokens[0] not in {"SELECT", "WITH", "PRAGMA"}:
        raise ValueError("only read-only SQL is allowed")
    if tokens[0] == "PRAGMA" and (len(tokens) < 2 or tokens[1] != "TABLE_INFO"):
        raise ValueError("only PRAGMA table_info is allowed in a read")
    if any(token in _DANGEROUS_SQL_TOKENS - {"PRAGMA"} for token in tokens):
        raise ValueError("SQL statement contains a disallowed operation")
    if any(token in _DANGEROUS_SQL_FUNCTIONS for token in tokens):
        raise ValueError("SQL statement contains a disallowed function")
    if "QUERY_ONLY" in tokens and "OFF" in tokens:
        raise ValueError("query_only cannot be disabled")


def validate_read_only_sql(sql: str, *, max_rows: int = SQL_MAX_ROWS) -> str:
    """Validate and wrap the deliberately limited CLI read-only SQL escape hatch."""

    if isinstance(max_rows, bool) or not isinstance(max_rows, int) or max_rows < 1 or max_rows > SQL_MAX_ROWS:
        raise ValueError(f"max_rows must be between 1 and {SQL_MAX_ROWS}")
    tokens = _sql_tokens(sql)
    if not tokens or tokens[0] not in {"SELECT", "WITH"}:
        raise ValueError("only SELECT or WITH read-only statements are allowed")
    if any(token in _DANGEROUS_SQL_TOKENS or token in _DANGEROUS_SQL_FUNCTIONS for token in tokens):
        raise ValueError("SQL statement contains a disallowed operation")
    if tokens[0] == "WITH" and "SELECT" not in tokens:
        raise ValueError("WITH statement must contain a SELECT")

    statement = sql.strip()
    if statement.endswith(";"):
        statement = statement[:-1].rstrip()
    return f"SELECT * FROM ({statement}) AS _kakaotalk_read_limit LIMIT {max_rows};"


# ─── Config ──────────────────────────────────────────────────────────────────

def load_config() -> dict:
    if os.path.exists(CONFIG_PATH):
        try:
            with open(CONFIG_PATH) as f:
                config = json.load(f)
            return config if isinstance(config, dict) else {}
        except (OSError, json.JSONDecodeError):
            return {}
    return {}


def save_config(cfg: dict):
    config_dir = os.path.dirname(CONFIG_PATH) or "."
    os.makedirs(config_dir, exist_ok=True)
    fd, temp_path = tempfile.mkstemp(prefix="config.", suffix=".tmp", dir=config_dir)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(cfg, f, indent=2)
            f.write("\n")
        os.chmod(temp_path, 0o600)
        os.replace(temp_path, CONFIG_PATH)
    finally:
        if os.path.exists(temp_path):
            os.unlink(temp_path)


def _discovered_db_for_config(configured_path: str | None) -> str | None:
    """Refresh discovery when the saved DB disappeared or another DB is live."""

    databases = find_db_files()
    if not databases:
        return configured_path if configured_path and os.path.isfile(configured_path) else None

    by_path = {entry["path"]: entry for entry in databases}
    configured = by_path.get(configured_path or "")
    active = next((entry for entry in databases if entry["has_wal"]), None)
    if configured and (not active or configured["has_wal"]):
        return configured["path"]
    if active:
        return active["path"]
    if configured:
        return configured["path"]
    return databases[0]["path"]


def _resolve_user_id(uuid: str, db_path: str, configured_user_id: Any) -> int:
    target = os.path.basename(db_path)
    candidate: int | None = None
    if configured_user_id is not None:
        try:
            candidate = int(configured_user_id)
        except (TypeError, ValueError):
            candidate = None
        if candidate and get_database_name(candidate, uuid) == target:
            return candidate

    plist_user_id = extract_user_id_from_plist()
    if plist_user_id and get_database_name(plist_user_id, uuid) == target:
        return plist_user_id

    brute_forced = bruteforce_user_id(uuid, target)
    if brute_forced is None:
        raise RuntimeError("KakaoTalk user ID could not be resolved for the selected database")
    return brute_forced


def resolve(args) -> tuple[str, str]:
    """Resolve db_path and secure_key."""
    cfg = load_config()
    uuid = getattr(args, "uuid", None) or cfg.get("uuid") or get_hardware_uuid()
    requested_user_id = getattr(args, "user_id", None)
    configured_user_id = requested_user_id if requested_user_id is not None else cfg.get("user_id")
    requested_db_path = getattr(args, "db_path", None)
    configured_db_path = requested_db_path if requested_db_path else cfg.get("db_path")

    if not uuid:
        raise RuntimeError("hardware identity could not be resolved")

    db_path = configured_db_path if requested_db_path else _discovered_db_for_config(configured_db_path)
    if not db_path or not os.path.isfile(db_path):
        raise RuntimeError("no KakaoTalk database file was found")

    user_id = _resolve_user_id(uuid, db_path, configured_user_id)

    resolved_config = {"uuid": uuid, "user_id": user_id, "db_path": db_path}
    if cfg != resolved_config:
        save_config(resolved_config)
    return db_path, get_secure_key(user_id, uuid)


def fmt_ts(ts) -> str:
    if not ts:
        return ""
    try:
        val = int(ts)
        if val == 0:
            return ""
        dt = datetime.fromtimestamp(val if val < 1e12 else val / 1000, tz=KST)
        return dt.strftime("%Y-%m-%d %H:%M:%S")
    except (ValueError, OSError, OverflowError):
        return str(ts)


# ─── Commands ────────────────────────────────────────────────────────────────

def cmd_info(args):
    uuid = get_hardware_uuid()
    cfg = load_config()
    dbs = find_db_files()
    plist_uid = extract_user_id_from_plist()

    if args.format == "json":
        print(json.dumps({"hardware_uuid": uuid, "plist_user_id": plist_uid,
                          "config": cfg, "db_files": dbs}, indent=2, ensure_ascii=False))
    else:
        print(f"Hardware UUID: {uuid}")
        print(f"sqlcipher: {SQLCIPHER_BIN} ({'OK' if os.path.exists(SQLCIPHER_BIN) else 'NOT FOUND'})")
        print(f"userId (plist): {plist_uid or 'not found'}")
        print(f"Config: {CONFIG_PATH}")
        if cfg:
            print(f"  user_id: {cfg.get('user_id')}")
            print(f"  db: {os.path.basename(cfg.get('db_path', ''))}")
        print(f"\nDB files ({len(dbs)}):")
        for d in dbs:
            tag = " [ACTIVE]" if d["has_wal"] else ""
            print(f"  {d['name'][:50]}  {d['size']:>10,}B{tag}")


def cmd_setup(args):
    uuid = getattr(args, "uuid", None) or get_hardware_uuid()
    if not uuid:
        print("Error: no hardware UUID.", file=sys.stderr); sys.exit(1)
    print(f"UUID: {uuid}")

    dbs = find_db_files()
    if not dbs:
        print("Error: no DB files.", file=sys.stderr); sys.exit(1)

    db_path = getattr(args, "db_path", None) or dbs[0]["path"]
    target = os.path.basename(db_path)
    print(f"DB: {target}")

    user_id = getattr(args, "user_id", None)
    if not user_id:
        user_id = extract_user_id_from_plist()
        if user_id and get_database_name(user_id, uuid) == target:
            print(f"userId (from plist): {user_id}")
        else:
            user_id = None
    if not user_id:
        user_id = bruteforce_user_id(uuid, target, args.max_id)
        if not user_id:
            print("Error: userId not found.", file=sys.stderr); sys.exit(1)

    print(f"User ID: {user_id}")
    key = get_secure_key(user_id, uuid)
    try:
        rows = query_json(db_path, key, "SELECT count(*) as cnt FROM sqlite_master;")
        print(f"Decryption OK! ({rows[0]['cnt']} objects)")
    except Exception as e:
        print(f"Decryption failed: {e}", file=sys.stderr); sys.exit(1)

    save_config({"uuid": uuid, "user_id": user_id, "db_path": db_path})
    print(f"Saved to {CONFIG_PATH}")


def cmd_tables(args):
    p, k = resolve(args)
    rows = query_json(p, k, "SELECT name, type FROM sqlite_master WHERE type IN ('table','view') ORDER BY name;")
    if args.format == "json":
        print(json.dumps(rows, indent=2))
    else:
        for r in rows:
            print(f"  {r['type']:6s} {r['name']}")


def cmd_schema(args):
    p, k = resolve(args)
    if args.table:
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", args.table):
            raise ValueError("table must be a simple identifier")
        rows = query_json(p, k, f"PRAGMA table_info({args.table});")
        if args.format == "json":
            print(json.dumps(rows, indent=2))
        else:
            for r in rows:
                print(f"  {r.get('name',''):30s} {r.get('type','')}")
    else:
        rows = query_json(p, k, "SELECT name, sql FROM sqlite_master WHERE type='table' ORDER BY name;")
        if args.format == "json":
            print(json.dumps(rows, indent=2, ensure_ascii=False))
        else:
            for r in rows:
                print(f"\n── {r['name']} ──\n  {r.get('sql','')}")


def _resolve_room_name(p: str, k: str, row: dict) -> str:
    """Resolve a display name for a chat room."""
    # 1) chatName if set
    if row.get("chatName"):
        return row["chatName"]

    # 2) For 1:1 or bot chats, use directChatMemberUserId
    direct_uid = row.get("directChatMemberUserId", 0)
    lid = row.get("linkId", 0)
    if direct_uid:
        try:
            users = query_json(p, k,
                "SELECT nickName FROM NTUser "
                "WHERE userId = :direct_user_id AND linkId = :link_id LIMIT :limit;",
                params={"direct_user_id": int(direct_uid), "link_id": int(lid or 0), "limit": 1},
                max_rows=1,
            )
            if users and users[0].get("nickName"):
                return users[0]["nickName"]
        except Exception:
            pass

    # 3) Fallback: gather member names from messages
    cid = row["chatId"]
    try:
        members = query_json(p, k,
            "SELECT u.nickName FROM ("
            "  SELECT DISTINCT authorId FROM NTChatMessage "
            "  WHERE chatId = :chat_id LIMIT :member_limit"
            ") a JOIN NTUser u ON a.authorId = u.userId AND u.linkId = :link_id "
            "LIMIT :name_limit;",
            params={"chat_id": int(cid), "member_limit": 20, "link_id": int(lid or 0), "name_limit": 10},
            max_rows=10,
        )
        names = [m["nickName"] for m in members if m.get("nickName")]
        if names:
            return ", ".join(names[:4])
    except Exception:
        pass

    return "(unnamed)"


def cmd_rooms(args):
    from kakaotalk_service import KakaoTalkService, serialize_json

    rows = KakaoTalkService().list_rooms()

    if args.format == "json":
        print(serialize_json(rows))
    else:
        for r in rows:
            t = r.get("roomType", "unknown")
            name = r.get("displayName", "(unnamed)")
            cid = r["chatId"]
            cnt = r.get("activeMembersCount", 0)
            suffix = f" ({cnt}명)" if isinstance(cnt, int) and cnt > 2 else ""
            print(f"  [{t:10s}] {cid} | {name}{suffix}")


def cmd_read(args):
    from kakaotalk_service import KakaoTalkService, serialize_json

    rows = KakaoTalkService().read_messages(room_id=args.room_id, limit=args.limit)

    if args.format == "json":
        print(serialize_json(rows))
    else:
        print(f"Messages: {len(rows)}\n")
        _print_msgs(rows)


def cmd_search(args):
    from kakaotalk_service import KakaoTalkService, serialize_json

    rows = KakaoTalkService().search_messages(keyword=args.keyword, limit=args.limit)

    if args.format == "json":
        print(serialize_json(rows))
    else:
        print(f"Search '{args.keyword}': {len(rows)} results\n")
        _print_msgs(rows, show_room=True)


def _parse_date(s: str) -> datetime | None:
    """Parse date string like '2026-03-18', 'today', 'yesterday', '-3d'."""
    if not s:
        return None
    s = s.strip().lower()
    now = datetime.now(tz=KST)
    if s == "today":
        return now.replace(hour=0, minute=0, second=0, microsecond=0)
    if s == "yesterday":
        return (now - timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
    if s.startswith("-") and s.endswith("d"):
        try:
            days = int(s[1:-1])
            return (now - timedelta(days=days)).replace(hour=0, minute=0, second=0, microsecond=0)
        except ValueError:
            pass
    try:
        dt = datetime.strptime(s, "%Y-%m-%d").replace(tzinfo=KST)
        return dt
    except ValueError:
        pass
    print(f"Invalid date: {s} (use YYYY-MM-DD, today, yesterday, -3d)", file=sys.stderr)
    sys.exit(1)


def cmd_recent(args):
    from kakaotalk_service import KakaoTalkService, serialize_json

    rows = KakaoTalkService().recent_messages(
        limit=args.limit,
        since=getattr(args, "since", None),
        until=getattr(args, "until", None),
    )

    if args.format == "json":
        print(serialize_json(rows))
    else:
        label = f"Recent {len(rows)} messages"
        if args.since:
            label += f" since {args.since}"
        if args.until:
            label += f" until {args.until}"
        print(f"{label}\n")
        _print_msgs(rows, show_room=True)


def cmd_users(args):
    from kakaotalk_service import KakaoTalkService, serialize_json

    rows = KakaoTalkService().list_users(limit=args.limit)

    if args.format == "json":
        print(serialize_json(rows))
    else:
        print(f"Users: {len(rows)}\n")
        for r in rows:
            uid = r.get("userId", "?")
            nick = r.get("nickName", "")
            did = r.get("directChatId", "")
            print(f"  {uid} | {nick}" + (f" | chat:{did}" if did else ""))


def cmd_poll(args):
    """Run one durable, read-only polling tick or acknowledge its batch."""
    from kakaotalk_poll import KakaoTalkPoller
    from kakaotalk_service import KakaoTalkService, serialize_json

    poller_kwargs = {"service": KakaoTalkService()}
    if args.state_file:
        poller_kwargs["state_file"] = args.state_file
    poller = KakaoTalkPoller(**poller_kwargs)
    if args.ack is not None:
        result = poller.acknowledge(args.ack, room_id=args.room_id)
    else:
        result = poller.poll_once(limit=args.limit, room_id=args.room_id)
    print(serialize_json(result))


def cmd_web(args):
    """Create secure web config locally or launch the headless web server."""
    from kakaotalk_web.config import (
        AUTH_MODE_IDENTITY,
        write_config,
    )

    if args.init_config:
        logins = args.login or []
        if args.auth_mode == AUTH_MODE_IDENTITY and not logins:
            raise ValueError("identity mode requires at least one --login")
        if args.auth_mode != AUTH_MODE_IDENTITY and logins:
            raise ValueError("network mode does not accept --login")
        write_config(
            args.config,
            allowed_logins=logins,
            port=args.port,
            refresh_seconds=args.refresh_seconds,
            auth_mode=args.auth_mode,
        )
        print("Web config created with mode 0600.")
        return

    from kakaotalk_web.config import load_settings
    from kakaotalk_web.server import run_server

    settings = load_settings(args.config)
    run_server(settings)


def cmd_sql(args):
    try:
        safe_query = validate_read_only_sql(args.query)
        p, k = resolve(args)
        rows = query_json(p, k, safe_query, max_rows=SQL_MAX_ROWS)
        if args.format == "json":
            print(json.dumps(rows, indent=2, ensure_ascii=False))
        else:
            if rows and isinstance(rows[0], dict):
                cols = list(rows[0].keys())
                print(" | ".join(cols))
                print("-" * min(120, len(" | ".join(cols))))
                for r in rows:
                    print(" | ".join(str(r.get(c, ""))[:80] for c in cols))
    except Exception as e:
        print(f"SQL error: {e}", file=sys.stderr)
        sys.exit(1)


def cmd_find_userid(args):
    uuid = getattr(args, "uuid", None) or get_hardware_uuid()
    if not uuid:
        print("Error: no hardware UUID.", file=sys.stderr); sys.exit(1)

    db_path = getattr(args, "db_path", None)
    if not db_path:
        dbs = find_db_files()
        if not dbs:
            print("No DB files.", file=sys.stderr); sys.exit(1)
        db_path = dbs[0]["path"]

    target = os.path.basename(db_path)
    print(f"UUID: {uuid}", file=sys.stderr)
    print(f"Target: {target}", file=sys.stderr)

    # Try plist first
    plist_uid = extract_user_id_from_plist()
    if plist_uid and get_database_name(plist_uid, uuid) == target:
        print(f"Found from plist! userId = {plist_uid}")
        return

    uid = bruteforce_user_id(uuid, target, args.max_id)
    if uid:
        print(json.dumps({"user_id": uid}) if args.format == "json" else f"Found! userId = {uid}")
    else:
        print(f"Not found in 1..{args.max_id}", file=sys.stderr); sys.exit(1)


def cmd_export(args):
    """Export decrypted DB as a plain SQLite file."""
    p, k = resolve(args)
    out = args.output or os.path.join(os.getcwd(), "kakaotalk.db")

    if os.path.exists(out) and not args.force:
        print(f"Error: {out} already exists. Use --force to overwrite.", file=sys.stderr)
        sys.exit(1)

    if os.path.exists(out):
        os.remove(out)

    # Use sqlcipher's ATTACH + sqlcipher_export to produce a plain SQLite DB
    sql = f"""
PRAGMA cipher_default_compatibility = 3;
PRAGMA KEY={_sql_literal(k)};
ATTACH DATABASE {_sql_literal(out)} AS plaintext KEY '';
SELECT sqlcipher_export('plaintext');
DETACH DATABASE plaintext;
"""
    r = subprocess.run([SQLCIPHER_BIN, p], input=sql,
                       capture_output=True, text=True, timeout=60)
    if r.returncode != 0 or not os.path.exists(out):
        print(f"Export failed: {r.stderr.strip()}", file=sys.stderr)
        sys.exit(1)

    size = os.path.getsize(out)
    print(f"Exported: {out} ({size:,} bytes)")
    print(f"\nCowork에서 사용하려면:")
    print(f"  1. 이 파일이 있는 폴더를 Cowork에 마운트")
    print(f"  2. sqlite3 kakaotalk.db 로 쿼리")
    print(f"\n주요 테이블:")
    print(f"  NTChatRoom    — chatId, type, chatName, linkId, lastLogId")
    print(f"  NTChatMessage — chatId, logId, authorId, type, sentAt, message, attachment")
    print(f"  NTUser        — userId, linkId, nickName")


# ─── Message display ────────────────────────────────────────────────────────

MSG_TYPES = {
    1: "", 2: "[photo]", 3: "[video]", 4: "[audio]",
    5: "[contact]", 6: "[file]", 12: "[sticker]",
    14: "[map]", 16: "[schedule]", 18: "[voip]",
    22: "[emoji]", 23: "[shout]", 24: "[reply]",
    26: "[gift]", 27: "[system]", 51: "[liveview]",
    71: "[voicetalk]", 72: "[grouptalk]",
}


def _print_msgs(rows: list[dict], show_room: bool = False):
    for m in rows:
        ts = fmt_ts(m.get("sentAt"))
        nick = m.get("nickName") or str(m.get("authorId", "?"))
        text = m.get("message") or ""
        mtype = int(m.get("type", 1))
        tag = MSG_TYPES.get(mtype, f"[type:{mtype}]") if mtype != 1 else ""

        room = f"[{m['chatId']}] " if show_room and "chatId" in m else ""

        # For non-text types, try to show attachment info
        if not text and tag and m.get("attachment"):
            try:
                att = json.loads(m["attachment"])
                text = att.get("url") or att.get("name") or att.get("k") or str(att)[:100]
            except (json.JSONDecodeError, TypeError):
                text = str(m["attachment"])[:100]

        line = f"[{ts}] {room}{nick}: {text}" + (f" {tag}" if tag else "")
        print(line)


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        prog="kakaotalk-reader",
        description="Read KakaoTalk Mac's encrypted local database.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s info                     Show config & DB files
  %(prog)s setup                    Auto-detect userId & verify
  %(prog)s rooms                    List chat rooms
  %(prog)s read <room_id> -n 100    Read messages from a room
  %(prog)s search "keyword" -n 20   Search messages
  %(prog)s recent -n 50             Recent 50 messages
  %(prog)s recent --since today     Today's messages
  %(prog)s recent --since yesterday --until yesterday
                                    Yesterday only
  %(prog)s recent --since 2026-03-01 --until 2026-03-07
                                    Date range
  %(prog)s recent --since="-3d"     Last 3 days
  %(prog)s users                    List users/contacts
  %(prog)s poll --state-file <path> Run one bounded polling tick
  %(prog)s poll --ack <token>       Commit a successfully processed batch
  %(prog)s web --init-config --login <identity>
                                    Create local mode-0600 web config
  %(prog)s web --config <path>       Start the loopback-only web server
  %(prog)s sql "SELECT ..."         Guarded read-only SQL query
  %(prog)s --format json recent     JSON output for piping

Requires: brew install sqlcipher
""",
    )
    parser.add_argument("--uuid", help="Hardware UUID (auto-detected)")
    parser.add_argument("--user-id", type=int, help="KakaoTalk userId (auto-detected from plist)")
    parser.add_argument("--db-path", help="DB file path (auto-detected)")
    parser.add_argument("--format", choices=["text", "json"], default="text")

    sub = parser.add_subparsers(dest="command")

    sub.add_parser("info", help="Show config and DB files")

    p = sub.add_parser("setup", help="Auto-detect and verify setup")
    p.add_argument("--max-id", type=int, default=2_000_000)

    p = sub.add_parser("find-userid", help="Find userId")
    p.add_argument("--max-id", type=int, default=2_000_000)

    sub.add_parser("tables", help="List DB tables")

    p = sub.add_parser("schema", help="Show table schema")
    p.add_argument("table", nargs="?")

    sub.add_parser("rooms", help="List chat rooms")

    p = sub.add_parser("read", help="Read messages")
    p.add_argument("room_id", nargs="?", type=int)
    p.add_argument("-n", "--limit", type=int, default=50)

    p = sub.add_parser("search", help="Search messages")
    p.add_argument("keyword")
    p.add_argument("-n", "--limit", type=int, default=50)

    p = sub.add_parser("recent", help="Recent messages")
    p.add_argument("-n", "--limit", type=int, default=30)
    p.add_argument("--since", help="Start date (e.g. 2026-03-18, today, yesterday)")
    p.add_argument("--until", help="End date (e.g. 2026-03-19, today)")

    p = sub.add_parser("users", help="List users/contacts")
    p.add_argument("-n", "--limit", type=int, default=100)

    p = sub.add_parser("poll", help="Run one bounded durable polling tick")
    p.add_argument("--state-file", help="Polling state path (default: user-local state)")
    p.add_argument("--limit", type=int, default=50)
    p.add_argument("--room-id", type=int)
    p.add_argument("--ack", metavar="TOKEN", help="Acknowledge the exact pending cursor")

    p = sub.add_parser("web", help="Run the headless read-only web viewer")
    p.add_argument("--config", help="Secure web config path (default: user-local web config)")
    p.add_argument("--init-config", action="store_true", help="Create a local mode-0600 config and exit")
    p.add_argument("--login", action="append", help="Allow one Tailscale identity; repeat for more than one")
    p.add_argument("--auth-mode", choices=["identity", "network"], default="identity")
    p.add_argument("--port", type=int, default=8765)
    p.add_argument("--refresh-seconds", type=int, default=15)

    p = sub.add_parser("sql", help="Run guarded read-only SQL")
    p.add_argument("query")

    args = parser.parse_args()
    if not args.command:
        parser.print_help(); sys.exit(1)

    cmds = {
        "info": cmd_info, "setup": cmd_setup, "find-userid": cmd_find_userid,
        "tables": cmd_tables, "schema": cmd_schema, "rooms": cmd_rooms,
        "read": cmd_read, "search": cmd_search, "recent": cmd_recent,
        "users": cmd_users, "poll": cmd_poll, "sql": cmd_sql,
        "web": cmd_web,
    }
    try:
        cmds[args.command](args)
    except (RuntimeError, ValueError) as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
