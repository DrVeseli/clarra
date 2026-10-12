# Protocol and Operations

## CLI operations

Run one synchronization pass:

```sh
clarra sync <local> <[user@]host:/project/path>
```

Use `local:<project>` instead of an SSH remote for local testing. Add `--watch`
to perform an initial pass, react to filesystem changes, and sync at least
every 30 seconds. Standard output is one JSON `EngineEvent` per line. Transport
diagnostics inherited from `ssh` go to standard error.

Resolve all known heads for a conflicted path with bytes from a file, then run
one sync pass:

```sh
clarra resolve <local> <remote> <relative-path> <content-file>
```

The path must currently have at least two heads in local state. The selected
content replaces the working file and becomes a new merge revision.

Serve one binary protocol session over stdin/stdout:

```sh
clarra attach <project>
```

Show server diagnostics from the server host:

```sh
clarra status <project>
```

Add `--tui` for a live Vesper-themed dashboard. Press `r` to refresh immediately
and `q`, Escape, or Ctrl-C to leave:

```sh
clarra status <project> --tui
```

`status` opens and, if necessary, initializes the project. Its pretty-printed
JSON contains:

| Field | Meaning |
| --- | --- |
| `project_id` | Generated project UUID |
| `sequence` | Last ledger event sequence |
| `participants` | Server account names/IDs and their registered device counts |
| `devices` | Registered device count |
| `files` | Paths with at least one current non-deleted head |
| `revisions` | Recorded revision count |
| `conflicts` | Paths with more than one head |
| `pending_deliveries` | Sum of device entries still pending across revisions |
| `cache_bytes` | Bytes in regular files under `.sync/cache`, including partial uploads |
| `cache_limit` | Configured cache capacity in bytes |
| `storage_bytes` | Ledger, derived index, and cache bytes |
| `uploaded_bytes` | Content bytes accepted by the coordinator |
| `downloaded_bytes` | Content bytes served by the coordinator |

The dashboard also shows active file heads, participants and device counts,
metadata storage, conflicts, pending deliveries, and durable uploaded/downloaded
byte totals. Participant names resolve from the server operating-system account
when possible. The current protocol does not expose SSH key comments or live
session presence, so online state is shown as `not tracked`.

## Event notifications

`sync` emits newline-delimited JSON objects tagged by `type`:

```json
{"type":"connected"}
{"type":"file_published","path":"notes/today.md"}
{"type":"file_received","path":"notes/today.md"}
{"type":"conflict","path":"notes/today.md","revision_ids":["...","..."]}
{"type":"error","message":"..."}
```

`sync_once` returns ordinary failures to its caller. In watch mode, each sync
failure becomes an `error` event and watching continues; watcher setup failure
or disconnection still returns an error.

## Cache quota

The default server cache limit is 10 GiB. Set `CLARRA_CACHE_BYTES` in the
environment of `clarra attach` to a decimal byte limit:

```sh
CLARRA_CACHE_BYTES=1073741824 clarra attach /srv/clarra/team
```

The server checks current cache-directory bytes plus each incoming upload
chunk. There is no eviction policy: capacity errors reject the chunk while the
ledger retains pending delivery metadata. Free capacity by allowing recipients
to sync and acknowledge content, or by increasing the limit. Manual cache
deletion is unsafe because the ledger is not automatically reconciled.

## Wire framing

Protocol version `1` runs over a single ordered, bidirectional byte stream.
Each frame is:

```text
4-byte unsigned big-endian payload length
postcard-encoded serde payload
```

Frames larger than 16 MiB are rejected. Content chunks are at most 256 KiB.
This is a Rust `postcard` representation of the implemented enums, not a
separately versioned JSON schema. Clients must send `Hello` first and exchange
exactly one response for each request.

Requests:

| Request | Purpose |
| --- | --- |
| `Hello { version, device_id, device_token }` | Negotiate version and authenticate or automatically register the device |
| `State` | Read heads, required uploads, available pending downloads, and conflicts |
| `Announce { revisions }` | Append up to 1,000 new revision metadata records |
| `UploadStatus { hash, total_size }` | Authorize an upload and obtain its resumable offset |
| `UploadChunk { hash, total_size, offset, bytes }` | Append one contiguous content chunk |
| `Download { hash, offset }` | Read the next content chunk for a pending delivery |
| `Acknowledge { revision_ids }` | Mark revisions delivered to this device and permit cache cleanup |

Responses:

| Response | Purpose |
| --- | --- |
| `Hello { project_id, sequence }` | Confirm the session and current ledger position |
| `State { sequence, heads, uploads, downloads, conflicts }` | Return synchronization work visible to the device |
| `UploadOffset { offset }` | Report the accepted/resumable upload position |
| `Chunk { offset, total_size, bytes }` | Return one download chunk |
| `Ok` | Confirm announce or acknowledgment |
| `Error { message }` | Reject one request while leaving the session available for another frame |

An upsert revision carries an ID, portable path, BLAKE3 hash, size, parent
revision IDs, author device, and `Upsert` operation. A deletion carries no
hash, has size zero, and uses `Delete`. Multiple heads for one path constitute
a conflict.
