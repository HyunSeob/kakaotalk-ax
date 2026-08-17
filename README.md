# kakaotalk-reader

KakaoTalk Mac local database reader, native MCP stdio bridge for Hermes, and an optional guarded Accessibility sender prototype.

V2 reads the encrypted SQLite database that KakaoTalk Mac keeps on the local Mac. The read plane does not use Accessibility/UI automation, AppleScript, network interception, credential extraction, or KakaoTalk message sending/deletion. The separately named outbound prototype described below is disabled by default and never writes the database.

## Requirements and setup

- macOS with KakaoTalk Mac installed and signed in
- Python 3.10 or newer
- SQLCipher (`brew install sqlcipher`)
- The Python MCP package for the bridge

Install the project with the MCP extra:

```bash
python3 -m pip install -e '.[mcp]'
```

For the optional headless browser viewer, install the web extra as well:

```bash
python3 -m pip install -e '.[web]'
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

## Headless Tailscale-only web viewer

The web viewer is a separate, read-only Starlette/uvicorn process. It runs on
the KakaoTalk Mac, queries the canonical `KakaoTalkService` in memory per
request. The encrypted database, WAL/SHM files, and any durable copies of
those source files remain on the source Mac. The server does not persist, copy
message content into a remote store, or cache it, and it creates no remote
database, cache, or export. An authorized remote browser receives only bounded
response data over the Tailscale path. It has no local GUI client.

The production launcher requires a local mode-0600 configuration file and
binds only to `127.0.0.1`. The default local port is `8765`, separate from the
existing Hermes service. Create the config locally; the command prints no
identity values:

```bash
kakaotalk-reader web --init-config \
  --config ~/.config/kakaotalk-reader/web.json \
  --login <tailscale-login>
```

The default `identity` mode requires exactly one Tailscale Serve-injected
`Tailscale-User-Login` header per request and checks it against the local
allowlist. Missing, repeated, malformed, or unapproved identities are denied
with generic responses. The explicit `network` mode omits identity-header
authentication and relies on Tailscale Serve plus the tailnet ACL as the
network boundary; create it only when that boundary is intentional:

```bash
kakaotalk-reader web --init-config \
  --config ~/.config/kakaotalk-reader/web.json \
  --auth-mode network
```

Start the collector/server from the checkout or installed entry point:

```bash
# After installing the project:
kakaotalk-reader web --config ~/.config/kakaotalk-reader/web.json

# Or directly from this checkout:
PATH=/opt/homebrew/bin:$PATH PYTHONPATH=src python3.13 -m kakaotalk_reader web \
  --config ~/.config/kakaotalk-reader/web.json
```

This process is a headless collector/server, not a local reader window. With
the default configuration, the web surface remains read-only: bounded room,
message, literal-search, refresh, and database/WAL-status reads. `/healthz`
returns only `{"ok":true}`. There are no users, context, updates, export, SQL,
delete, CORS, cache, or API-doc routes. The optional outbound routes below are
present but cannot invoke Accessibility while outbound is disabled.

### Guarded macOS Accessibility outbound prototype

The outbound namespace is a prototype adapter for the official KakaoTalk Mac
client, not an official Kakao integration or protocol client. It is disabled
by default, is not registered as a Hermes/MCP tool, and never writes the
KakaoTalk database, WAL, or SHM. It supports bounded text only. It does not
navigate to a room, retry, schedule, send media, or expose the helper on a
socket.

The web contract is deliberately two-step and uses the same authentication
middleware as the read API:

1. `POST /api/v1/outbound/preview` validates an exact room ID/display identity
   using the bounded read service and creates a 60-second in-memory draft.
   Preview never contacts Accessibility. It refuses when the 100-room lookup
   is full or when the display identity is duplicated, because the window
   title would not prove the selected room.
2. `POST /api/v1/outbound/confirm` accepts only that one-time draft ID and the
   exact text `CONFIRM SEND`. A valid attempt consumes the draft before it
   checks the enabled gate, revalidates the target, or invokes the helper.

The helper uses macOS AX APIs directly. Prepare requires KakaoTalk to be the
frontmost app, the focused window title to exactly equal the expected room
identity, and exactly one enabled writable `AXTextArea`. It writes the text
and reads it back byte-for-byte. Send is a separate helper operation that
rechecks the window, composer value, and a unique enabled `Send` or `전송`
button before `AXPress`. Any ambiguous hierarchy, title variation, missing AX
permission, readback mismatch, or changed UI fails closed. Labels and the AX
hierarchy are version-sensitive.

Build the helper without running it or contacting KakaoTalk:

```bash
HOME="$PWD/.swift-home" \
CLANG_MODULE_CACHE_PATH="$PWD/.swift-module-cache" \
SWIFTPM_MODULECACHE_OVERRIDE="$PWD/.swift-module-cache" \
swift build --disable-sandbox --package-path ax-helper -c release
```

Install the resulting binary to an owner-only path manually, for example:

```bash
install -m 700 ax-helper/.build/release/kakaotalk-ax-helper \
  ~/.local/bin/kakaotalk-ax-helper
```

Only the non-mutating availability check is suitable for automated/manual
setup verification:

```bash
~/.local/bin/kakaotalk-ax-helper availability
```

It returns only `{"status":"available"}` or
`{"status":"unavailable"}` and never prompts for permission. Private room
identity and message text are passed to the helper over stdin, not command-line
arguments. An unavailable configured helper does not prevent the read API
from starting. Before every helper invocation, the web process requires an
absolute, owner-owned regular executable with no group/world permissions and
a non-writable, symlink-free root/user-owned directory chain. It launches the helper locally
with a small fixed environment, bounded response schema, and timeout; there is
no network listener.

Enabling is an explicit manual edit to the existing mode-0600 web config:

```json
{
  "allowedLogins": ["<tailscale-login>"],
  "authMode": "identity",
  "outbound": {
    "enabled": true,
    "helperPath": "/absolute/owner-only/path/kakaotalk-ax-helper"
  },
  "port": 8765,
  "refreshSeconds": 15
}
```

Before any future explicitly authorized live send, KakaoTalk must be running
and signed in; the intended existing conversation must already be open and
frontmost; its accessible window title must exactly match the selected viewer
room; the composer must be empty or safe to replace; macOS Accessibility
permission must have been granted to the helper; the helper must report
available; and the operator must inspect the preview and type the exact
confirmation value. A successful AX press returns `uncertain`, never
“delivered.” Verify through the existing read-only room messages endpoint or
poller. Do not retry automatically if verification times out.

### Manual Tailscale Serve step

This checkout does not configure Tailscale. Before changing Serve, the
operator must inspect and save the existing configuration, including the
Hermes root mount. Start the web process, choose an unused HTTPS listener or
path, apply the corresponding Serve command manually, and verify afterward
that the Hermes root remains intact:

```bash
# Inspect and save the current configuration before making a change.
tailscale serve status
kakaotalk-reader web --config ~/.config/kakaotalk-reader/web.json
# Choose an unused tailnet port after inspection and apply this manually.
tailscale serve --https=<unused-tailnet-port> --bg 8765
```

Do not assume that the example command preserves existing Serve mounts; its
behavior must be checked against the installed CLI and saved configuration.
Replace the placeholder with an unused tailnet port after inspection, then
verify the resulting Serve configuration and the Hermes root. The browser URL
shape is:

```text
https://<tailscale-hostname>:<unused-tailnet-port>/
```

If a path mount is preferred, choose an unused path and use the installed
CLI's documented `--set-path` form after reviewing and saving the existing
mounts, for example `tailscale serve --set-path=/kakaotalk --bg 8765`. Verify
the Hermes root after applying it; do not use `reset` or `clear` for this
setup. Do not run either form automatically from this project.

Tailscale Serve injects identity headers for tailnet user traffic and removes
client-supplied copies before proxying; tagged-device traffic does not receive
user identity headers. This uses Serve's built-in user identity behavior; no
`--accept-app-caps` flag is needed for `Tailscale-User-Login` (that flag is for
the separate app-capabilities header). The app therefore keeps loopback
binding mandatory and does not trust `X-Forwarded-*` or a direct client header
on a non-proxied interface. Use `network` mode only for a deliberately
reviewed ACL boundary or when the connecting device has no user identity
header. See the [Tailscale Serve identity-header documentation](https://tailscale.com/docs/features/tailscale-serve#identity-headers).

Remote access is not claimed until an operator performs a real tailnet
verification: check the Serve mount, open the placeholder URL from a separate
tailnet device, confirm `/healthz`, confirm the intended identity behavior,
and confirm that the existing Hermes root still serves its original service.
No live Serve change was made while implementing this viewer.

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

This is the existing MCP/service cursor contract. The V2.1 durable unscoped
poller does not use one global `kt2` watermark; it uses the bounded per-room
`ktg1` contract below so a lower-numbered room cannot be skipped.

## V2.1 durable polling and live WAL behavior

V2.1 adds a bounded, one-shot local poller. It is not a daemon and does not
register a Hermes cron job or webhook. The MCP surface remains unchanged; the
poller calls the same named V2 service methods from a local process:

```bash
kakaotalk-reader poll --state-file ~/.config/kakaotalk-reader/poll-state.json --limit 25
```

The default state path is
`~/.config/kakaotalk-reader/poll-state.json`; `--state-file` is available for
tests and for a future Hermes scheduler. A separate state file should be used
for each `--room-id` scope. The directory is created/enforced as mode `0700`
and the state file as mode `0600`.

### Poll envelope and cursor/ack contract

Every successful poll emits one compact JSON object with exactly these keys:

```json
{
  "state": "bootstrapped|unchanged|updated",
  "messages": [],
  "hasMore": false,
  "nextCursor": "ktg1.<bounded-room-cursor>",
  "ackRequired": false,
  "ackToken": null
}
```

`state` is `bootstrapped` for a new state file, `unchanged` when the persisted
database/WAL signatures are unchanged and there is no pending work, and
`updated` when a source change was read or a pending batch was retried.
`messages` and `hasMore` come from bounded, room-scoped V2 updates calls.
When `--room-id` is supplied, `nextCursor` is the existing `kt2` cursor for
that room. With no room scope, `nextCursor` is a `ktg1` cursor containing only
the bounded numeric per-room watermarks; it is a poller token, not an input to
the MCP `kakaotalk_updates` tool. `nextCursor` is the committed cursor for
bootstrap/unchanged results and the pending next cursor for an update. When
messages are returned, `ackRequired` is true and `ackToken` is the exact same
cursor string.

The first poll never replays the historical database. A scoped poll reads one
numeric high-water row for that room. An unscoped poll discovers up to the
explicit `MAX_GLOBAL_ROOMS=384` usable room IDs, reads all high-water rows in
one bounded bulk query, then stores a `ktg1` token and returns
`messages: []`. Empty rooms are retained with a null per-room watermark so
their first later message is discoverable. Room-table rows with non-positive or
non-integer-like IDs are filtered before the bounded discovery result; invalid
message ordering rows remain fail-closed.

The state JSON is deliberately limited to version, optional numeric room
scope, either a scoped `kt2` cursor or an encoded `ktg1` list of bounded
numeric room cursors, bounded `(size, mtime_ns)` signatures for the DB and
WAL, and bounded pending/continuation metadata. It never stores message text,
names, database/WAL paths, UUIDs, keys, credentials, absolute paths, or
attachments. Malformed or oversized state fails closed without echoing its
contents. State writes use a `0600` temporary file, flush/fsync where
available, and atomic replacement.

The canonical state envelope is:

```json
{
  "v": 1,
  "scope": null,
  "cursor": "ktg1.<bounded-room-cursor>",
  "signature": {"database": [123, 456], "wal": [78, 90]},
  "pending": null,
  "more": false
}
```

While a batch awaits acknowledgement, `pending` is
`{"cursor":"ktg1.<bounded-room-cursor>","signature":{"database":[123,456],"wal":[78,90]},"hasMore":false}`;
`cursor` remains the last committed cursor until acknowledgement. For an
unscoped poll, the token advances only the per-room cursors represented by the
acknowledged batch; rooms are merged by `(chatId, logId, msgId, revision)`
only after each room has been independently filtered by its own cursor. A
new message in a lower-numbered room therefore cannot be hidden by a higher
room's watermark.

The source signatures are persisted, so a new one-shot process can skip the
SQLCipher updates query when neither the database nor WAL signature changed.
If signatures changed, a scoped poll calls `kakaotalk_updates` with its
committed cursor and a limit of 1–100. An unscoped poll uses one bounded bulk
updates query with an independent numeric watermark for each of up to 384
rooms, then merges the bounded result by `(chatId, logId, msgId, revision)`.
The bulk query uses two bound parameters per room so the 384-room limit stays
below the stock SQLite variable limit; it does not launch one SQLCipher
process per room. A room absent from the initial room set starts with a null
cursor and is queried from the beginning; because it was not present during
the initial bootstrap, this does not replay rows from that bootstrap and
cannot skip its first post-discovery row. A legacy empty global state is the
exception: it is upgraded with one bulk per-room high-water read to retain the
old no-replay boundary. If a batch is returned, its cursor is pending and is
not committed merely because the read succeeded.
After the caller has successfully processed the messages, acknowledge that
exact token:

```bash
kakaotalk-reader poll --state-file ~/.config/kakaotalk-reader/poll-state.json \
  --ack '<ackToken-from-the-poll-envelope>'
```

Acknowledgement atomically advances the committed cursor. If processing fails,
the token is not acknowledged; the next tick queries the same old cursor and
retries the batch without persisting its contents. `hasMore: true` also forces
the next continuation query after acknowledgement even when file signatures
remain unchanged. A wrong or missing acknowledgement leaves the old cursor and
pending batch intact.

The existing V2 scoped state remains readable. A legacy unscoped state with a
non-null `kt2` cursor is rejected with an explicit migration error because a
single global `chatId`-first cursor cannot be converted without risking missed
messages; remove that state file to perform a fresh per-room bootstrap. A
legacy empty global state with a null cursor is upgraded on its next poll. The
new `ktg1` poller token must never be passed to the MCP update tool.

For comparison, the existing MCP status/update calls remain available to
callers that manage their own cursor:

```json
{"name":"kakaotalk_status","arguments":{}}
{"name":"kakaotalk_updates","arguments":{"cursor":"","limit":25}}
{"name":"kakaotalk_updates","arguments":{"cursor":"kt2.<returned-value>","limit":25}}
```

Each scoped update and high-water call still uses a fresh SQLCipher read-only
process and explicit read transaction. Global polling uses bounded bulk
high-water and watermark-filtered update reads, each still read-only and
parameterized. `NTChatRoom.lastLogId` is used as a per-room high-water
condition when that column is available. Optional V2 columns are discovered
with read-only `PRAGMA table_info` and omitted or replaced with safe fallbacks
when absent. A new message arriving between calls may be returned after the
previous cursor; cross-call historical snapshots are not promised.

### Future Hermes scheduling boundary

A future Hermes cron job can invoke one `poll` command per scheduled tick,
forward the bounded JSON messages through its own processing path, and invoke
the separate `--ack` command only after that processing succeeds. V2.1 does
not auto-push into Hermes: it adds no launchd installation, Hermes config
edit, webhook/network listener, message sender, or UI automation. Hermes
cron/webhook registration and delivery are the next integration step.

## V2 message normalization and privacy limits

V2 keeps the V1 fields and adds `msgId`, `revision`, `attachmentInfo`, and optional bounded `supplement`, `extra`, `thread`, `scope`, and `system` metadata when the database provides them. Structured JSON is returned under a small `{kind,value,truncated}` envelope; malformed or unknown values use `kind: "raw"` with a bounded fallback. Metadata is depth-, item-, and byte-bounded, non-finite JSON numbers are replaced, and local file-path-looking values are redacted. System type `27` is marked with `system.isSystem: true`.

V2 does not return derived keys, hardware identifiers, database/WAL/SHM paths, absolute local file paths, unbounded blobs, or arbitrary SQL results. It remains a local read bridge only; no KakaoTalk source file is copied, checkpointed, renamed, deleted, or modified.

## Architecture

```text
Hermes MCP stdio                    kakaotalk-reader poll
        |                                   |
src/kakaotalk_mcp.py                 src/kakaotalk_poll.py
        |                                   |
        +------ src/kakaotalk_service.py --+
                         |
                 src/kakaotalk_reader.py
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

- The CLI/MCP bridge remains local and read-only. It has no send, delete, edit, export, UI automation, network, or credential-extraction operation. The separately gated web outbound prototype is limited to the AX text flow documented above.
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
kakaotalk-reader poll --limit 25
kakaotalk-reader poll --ack <token>
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
