# kakaotalk-reader

Read-only KakaoTalk Mac local database reader and native MCP stdio bridge for Hermes.

V1 reads the encrypted SQLite database that KakaoTalk Mac keeps on the local Mac. It does not use Accessibility/UI automation, AppleScript, network interception, credential extraction, or KakaoTalk message sending/deletion.

## Requirements and setup

- macOS with KakaoTalk Mac installed and signed in
- Python 3.10 or newer
- SQLCipher (`brew install sqlcipher`)
- The Python MCP package for the bridge

Install the project with the MCP extra:

```bash
python3 -m pip install -e '.[mcp]'
```

Run the one-time local setup. It discovers the current encrypted database, derives the local key, and verifies that SQLCipher can read it:

```bash
kakaotalk-reader setup
```

The reader stores local discovery configuration at `~/.config/kakaotalk-reader/config.json` with mode `0600`. It contains sensitive local identifiers and must not be committed or copied into bug reports. The resolver re-checks database discovery on every operation and refreshes the saved database path when the configured file is missing or a different database has the active WAL file.

## Hermes MCP configuration

Add the following to Hermes' configuration. This is an example only; do not edit or overwrite `~/.hermes/config.yaml` automatically. Replace the source checkout path with the local absolute path.

```yaml
mcp_servers:
  kakaotalk:
    command: /opt/homebrew/bin/python3
    args:
      - /absolute/path/to/kakaotalk-reader/src/kakaotalk_mcp.py
```

The equivalent command for a manually managed stdio client is:

```bash
python3 /absolute/path/to/kakaotalk-reader/src/kakaotalk_mcp.py
```

The installed console entry point is also available after installing the `mcp` extra:

```bash
kakaotalk-mcp
```

The process speaks MCP over stdio. Do not put diagnostic output or shell wrappers on its stdout.

## MCP tools

The default surface contains only these read operations:

| Tool | Purpose | Bounded inputs |
|---|---|---|
| `kakaotalk_list_rooms` | List rooms, type labels, IDs, and display names | `limit` 1–100 |
| `kakaotalk_read_messages` | Read one room in chronological order | positive `room_id`; `limit` 1–100 |
| `kakaotalk_recent_messages` | Read recent messages with KST date filters | `limit` 1–100; `today`, `yesterday`, `-Nd`, or `YYYY-MM-DD` |
| `kakaotalk_search_messages` | Literal keyword search in message text | keyword ≤256 characters; `limit` 1–100 |
| `kakaotalk_list_users` | List known users using known columns only | `limit` 1–200 |
| `kakaotalk_message_context` | Return a message plus nearby messages in its room | positive room/message IDs; each radius 0–20 |

Every tool returns deterministic JSON text with stable keys, bounded text/attachment fields, human-readable `time` values in KST, `typeLabel`/`roomType` labels, and a room name when the database provides one. IDs are validated as positive integers. Invalid input fails before a database query.

There is intentionally no arbitrary SQL MCP tool. The legacy CLI `sql` command remains available only as a guarded local escape hatch for one `SELECT`/`WITH` statement, with write keywords/comments/multiple statements rejected and an outer 500-row cap:

```bash
kakaotalk-reader sql "SELECT chatId, chatName FROM NTChatRoom ORDER BY lastLogId DESC"
```

## Architecture

```text
Hermes MCP stdio
        |
src/kakaotalk_mcp.py       named tools, transport, safe errors
        |
src/kakaotalk_service.py  validation, SQL templates, normalization, bounds
        |
src/kakaotalk_reader.py    discovery, key derivation, SQLCipher process, CLI
        |
KakaoTalk encrypted SQLite + WAL/SHM files
```

The service selects known columns from `NTChatRoom`, `NTChatMessage`, and `NTUser`; it does not pass arbitrary MCP text into SQL. User strings are bound through SQLCipher CLI parameters. The CLI and MCP share the service so their read semantics stay aligned.

## Live database and WAL behavior

Each read starts a fresh SQLCipher process and uses:

1. SQLCipher's `-readonly` open mode and `PRAGMA query_only = ON` so the KakaoTalk database cannot be written by a read operation.
2. `PRAGMA busy_timeout` plus a short retry with a fresh process if KakaoTalk is briefly writing.
3. An explicit read transaction, giving that query a consistent SQLite/WAL snapshot.

The source database, `-wal`, and `-shm` files are not copied, checkpointed, renamed, or deleted. A compound operation such as message context performs a few read snapshots in one tool call, so a message arriving during that call can appear between portions of the result. Cross-call historical consistency is not promised.

## Privacy and security boundaries

- The bridge is local and read-only. It has no send, delete, edit, export, UI automation, network, or credential-extraction operation.
- The MCP process does not return the derived key, hardware UUID, configuration path, or SQLCipher diagnostics. Errors are generic enough not to echo message data or secrets.
- Message and user data are returned only because the caller explicitly requests a read tool. Results are row- and field-bounded before JSON serialization.
- Local configuration, databases, WAL/SHM files, exports, caches, and generated output are covered by `.gitignore`; `.gitignore` is only a guard, not a substitute for reviewing files before commit.
- Do not paste MCP output, local configuration, or database files into issues or logs.

## Existing CLI commands

```bash
kakaotalk-reader info
kakaotalk-reader setup
kakaotalk-reader rooms
kakaotalk-reader read <room_id> -n 100
kakaotalk-reader recent -n 30
kakaotalk-reader recent --since today
kakaotalk-reader recent --since yesterday --until yesterday
kakaotalk-reader recent --since 2026-03-01 --until 2026-03-07
kakaotalk-reader search "keyword" -n 50
kakaotalk-reader users
kakaotalk-reader --format json recent --since today
```

The pre-existing `find-userid`, `tables`, and `schema` commands remain available for setup and diagnostics.

## Tests and checks

The normal test suite uses Python's standard library and synthetic rows/mocks only. It does not require a real KakaoTalk database or SQLCipher:

```bash
PYTHONPATH=src python3 -m unittest discover -s tests -v
PYTHONPATH=src python3 -m compileall -q src tests
```

## Current V1 limitations

- SQLCipher and the current KakaoTalk Mac schema are required; schema changes may require updating the known-column templates.
- Automatic user-ID discovery can be slow when plist discovery cannot verify the active database.
- The resolver supports the current local profile/database discovery pattern, not multi-account selection UX.
- MCP results do not decode every attachment type and intentionally truncate large message/attachment fields.
- Date filters are KST calendar-day filters. They are not arbitrary timestamp filters.
- This is a read bridge, not a backup, sync, search index, or write-capable KakaoTalk client.

## Database tables used

| Table | Columns used by V1 |
|---|---|
| `NTChatRoom` | `chatId`, `type`, `chatName`, `linkId`, `lastLogId`, `directChatMemberUserId`, `activeMembersCount` |
| `NTChatMessage` | `chatId`, `logId`, `authorId`, `type`, `sentAt`, `message`, `attachment` |
| `NTUser` | `userId`, `linkId`, `nickName` |

The key-derivation approach is based on the publicly referenced KakaoTalk Mac research/code linked by the original project. It is used only to read the local database and never logs the derived key.
