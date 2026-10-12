# Architecture and Storage

## Components

Clarra has three runtime pieces:

- `Engine` scans a local directory, records revisions, transfers objects, and
  applies remote revisions.
- `clarra` exposes the engine as `sync` and `resolve`, and exposes the
  server as `attach` and `status`.
- The server-side ledger is an append-only metadata coordinator. Its cache is
  a delivery buffer, not a backup or primary file store.

An SSH sync starts `clarra attach` on demand. There is no daemon or
listening Clarra port. The client and server exchange binary messages over
the child process's stdin and stdout.

Each sync performs this sequence:

1. Open or create local metadata and send the stored device ID and token.
2. Read server heads, scan local regular files, and acknowledge matching files.
3. Announce local upserts and deletion tombstones.
4. Upload content requested for another registered device.
5. Download pending revisions, verify content, apply files or tombstones, and
   acknowledge delivery.

Concurrent heads are preserved as conflicts. A non-fast-forward remote upsert
is written beside the original as
`<stem>.clarra-conflict-<device>-<revision><extension>`; the existing file
is not overwritten. `resolve` creates one revision whose parents are all known
heads.

## Deliberate MVP choices

- SSH supplies transport encryption and access to the remote command; Clarra
  does not run an HTTP service or implement an application login.
- The server always uses its temporary content cache for delivery. Direct
  device-to-device or live stream relay is not implemented.
- Metadata is event-sourced in a JSON-lines ledger. The JSON index is derived
  and is not used to load authoritative state.
- BLAKE3-addressed objects and 256 KiB chunks provide verification and
  resumable partial uploads/downloads without a separate database.
- Watch mode combines recursive filesystem notifications with a 30-second
  sync interval and a 300 ms delay after relevant changes.

## Storage locations

For a local directory `<local>`:

```text
<local>/.clarra/state.json   device credentials, revisions, heads, outbox
<local>/.clarra/objects/     retained content-addressed objects
<local>/.clarra/tmp/         captures and partial downloads
```

For a server project `<project>`:

```text
<project>/.sync/ledger.log       authoritative append-only JSON-lines ledger
<project>/.sync/index/state.json derived state snapshot
<project>/.sync/cache/           complete objects and .partial uploads
<project>/.sync/lock             process lock for ledger operations
```

Opening a local directory automatically generates a UUID device ID and a
random token if `state.json` has no identity. The token is stored in plaintext
locally; only its BLAKE3 hash is recorded in the server ledger. Deleting local
`state.json` creates a new device on the next sync and loses that directory's
local revision bookkeeping.

The scanner recursively includes regular files. It skips symlinks,
`.clarra/`, generated names beginning with `.clarra-`, and generated
conflict copies. Paths must be relative UTF-8 paths and cannot contain `..`, a
backslash, NUL, or a first component of `.clarra`.

## Recovery

Ledger appends and local/index state replacements are flushed before success.
The ledger uses an exclusive file lock. On `Ledger::open`, an incomplete final
ledger line is truncated to the preceding newline; a malformed complete line,
sequence gap, or invalid event is an error and is not repaired automatically.

Partial transfers resume from their recorded byte length. Completed downloads
and uploads are accepted only after size and BLAKE3 verification. Cached server
objects are removed after no pending recipient needs their hash.

Back up `ledger.log` to preserve project identity, devices, history, and
pending deliveries. The server index can be reconstructed by a later ledger
append, but there is no standalone rebuild command. Cached objects and local
`objects/` directories are not backups: missing content is not reconstructed
unless an eligible holder can upload it on a future sync.

## Known MVP limitations

- No direct P2P transfer, live relay, server daemon, web API, or discovery.
- No end-to-end or cache-at-rest encryption; the SSH server can read cached
  content.
- No device administration, revocation command, explicit participant
  allowlist, or token rotation.
- No configurable ignores, selective sync, rename detection, or preservation
  of empty directories, permissions, ownership, extended attributes, or
  symlinks.
- Non-UTF-8 names are unsupported. Case-folding collisions and Unicode
  normalization differences are not reconciled. The simple `host:path` parser
  does not support IPv6 literal syntax or per-invocation SSH options.
- Watch mode has no dedicated reconnect/backoff state. A failed sync is emitted
  as an error event and another attempt occurs after a later change or timeout.
- The ledger, server index history, and local object store have no compaction or
  general garbage collection. There is no cache TTL or eviction policy.
- Concurrent `attach` processes can race while measuring cache usage, so the
  byte limit is not a strict reservation system.
- There is no automatic reconciliation for a ledger/cache mismatch caused by
  manual deletion or restoration of only part of `.sync/`.
- A newly registered recipient can make an existing revision pending, but a
  holder must sync again to stage the object before the recipient can fetch it.
