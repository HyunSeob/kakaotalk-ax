# kakaotalk-reader

Read-only KakaoTalk Mac local database reader and native MCP stdio bridge for Hermes.

V2 reads the encrypted SQLite database that KakaoTalk Mac keeps on the local Mac. It does not use Accessibility/UI automation, AppleScript, network interception, credential extraction, or KakaoTalk message sending/deletion.

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

Add the following to Hermes' configuration. This is an example only; do not edit or overwrite `~/.hermes/config.yaml` automatically. Replace `<checkout>` with the source checkout directory when configuring Hermes yourself.

```yaml
mcp_servers:
  kakaotalk:
    command: /opt/homebrew/bin/python3
    args:
      - <checkout>/src/kakaotalk_mcp.py
```

The equivalent command for a manually managed stdio client is:

```bash
python3 <checkout>/src/kakaotalk_mcp.py
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
| `kakaotalk_updates` | Read an incremental page after a stable cursor | cursor `≤256` chars; `limit` 1–100; optional positive `room_id` |
| `kakaotalk_status` | Report database availability and process-local WAL change state | no arguments |

Every tool returns deterministic JSON text with stable keys, bounded text/attachment/metadata fields, human-readable `time` values in KST, `typeLabel`/`roomType` labels, and a room name when the database provides one. IDs are validated as positive integers. Invalid input fails before a database query.

There is intentionally no arbitrary SQL MCP tool. The legacy CLI `sql` command remains available only as a guarded local escape hatch for one `SELECT`/`WITH` statement, with write keywords/comments/multiple statements rejected and an outer 500-row cap:

```bash
kakaotalk-reader sql "SELECT chatId, chatName FROM NTChatRoom ORDER BY lastLogId DESC"
```

## V2 incremental cursor contract

`kakaotalk_updates` returns an envelope with exactly these top-level fields:

```json
{"hasMore":true,"messages":[...],"nextCursor":"kt2.<base64url>"}
```

The empty cursor (`""`, or an omitted cursor in a direct service call) means the initial page. `nextCursor` is `null` when the initial page is empty; after a non-empty page it identifies the final returned row. When a supplied cursor produces no rows, the service returns that same cursor with `hasMore: false`.

The cursor is a versioned, canonical, padding-free base64url encoding of a small JSON object:

```json
{"k":[chatId,logId,msgId,revision],"scope":null,"v":1}
```

Only bounded numeric IDs are encoded. It contains no message text, names, paths, keys, secrets, or timestamps. The `kt2.` prefix and version field make future decoder changes explicit; the decoder also accepts the equivalent padded base64url spelling. Invalid, oversized, non-canonical, wrong-version, or wrong-room cursors fail before a database query.

Rows are ordered ascending by `(chatId, logId, msgId, revision)`. If `msgId` or `revision` is absent in the current schema, V2 uses `logId` and `0` respectively. The optional `room_id` is part of the cursor scope: reuse a cursor with the same room scope, or omit the scope for a global stream. A continuation uses a strict `>` comparison on the composite key, so an unchanged database snapshot does not repeat the returned row.

## V2 polling and live WAL behavior

The V2 MCP path does not create a daemon, watcher, index, export, or persistent cursor store. A bounded polling loop can:

1. Call `kakaotalk_status`.
2. Call `kakaotalk_updates` with `cursor: ""` initially, then with the returned `nextCursor`.
3. Poll again when `walPresent` is true and `walChanged` becomes true, or at the caller's chosen interval when the WAL is absent.

`kakaotalk_status` returns `databaseAvailable`, `walPresent`, `walChanged`, `databaseChanged`, and `changeKnown`. Change flags compare file size/mtime signatures held only in the running MCP process. The first result has `changeKnown: false`; restarting the process resets that comparison. Status does not disclose the database path, UUID, key, or raw diagnostics. `databaseAvailable` means the resolved database file is present; it is not a promise that decryption or every optional table is valid.

Each update call still uses a fresh SQLCipher read-only process and explicit read transaction. `NTChatRoom.lastLogId` is used as a per-room high-water condition when that column is available. Optional V2 columns are discovered with read-only `PRAGMA table_info` and omitted or replaced with safe fallbacks when absent. A new message arriving between calls may be returned after the previous cursor; cross-call historical snapshots are not promised.

Example MCP calls (the caller owns cursor persistence):

```json
{"name":"kakaotalk_status","arguments":{}}
{"name":"kakaotalk_updates","arguments":{"cursor":"","limit":25}}
{"name":"kakaotalk_updates","arguments":{"cursor":"kt2.<returned-value>","limit":25}}
```

## V2 message normalization and privacy limits

V2 keeps the V1 fields and adds `msgId`, `revision`, `attachmentInfo`, and optional bounded `supplement`, `extra`, `thread`, `scope`, and `system` metadata when the database provides them. Structured JSON is returned under a small `{kind,value,truncated}` envelope; malformed or unknown values use `kind: "raw"` with a bounded fallback. Metadata is depth-, item-, and byte-bounded, non-finite JSON numbers are replaced, and local file-path-looking values are redacted. System type `27` is marked with `system.isSystem: true`.

V2 does not return derived keys, hardware identifiers, database/WAL/SHM paths, absolute local file paths, unbounded blobs, or arbitrary SQL results. It remains a local read bridge only; no KakaoTalk source file is copied, checkpointed, renamed, deleted, or modified.

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

## Current V2 limitations

- SQLCipher and the current KakaoTalk Mac schema are required; schema changes may require updating the known-column templates.
- Automatic user-ID discovery can be slow when plist discovery cannot verify the active database.
- The resolver supports the current local profile/database discovery pattern, not multi-account selection UX.
- MCP results do not decode every attachment type; optional structured values use bounded metadata or a raw fallback.
- Date filters are KST calendar-day filters. They are not arbitrary timestamp filters.
- The cursor is incremental pagination over the current database ordering, not a durable backup, search index, or conflict-free replication log. Deletes, schema migrations, and edits that change ordering fields can affect later polling.
- Normal tests use synthetic rows and mocks only. No live KakaoTalk database or SQLCipher success is claimed by the test suite.

## Database tables used

| Table | Required/known columns |
|---|---|
| `NTChatRoom` | V1 fields plus optional feature-detected `lastLogId`, `chatName`, `linkId`, `directChatMemberUserId` |
| `NTChatMessage` | V1 fields plus optional feature-detected `attachment`, `supplement`, `extra`, `thread`, `revision`, `scope`, `msgId`, `systemMessage` |
| `NTUser` | `userId`, `linkId`, `nickName` |

The key-derivation approach is based on the publicly referenced KakaoTalk Mac research/code linked by the original project. It is used only to read the local database and never logs the derived key.
