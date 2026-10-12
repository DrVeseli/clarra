# Clarra — Technical Specification

**Status:** Initial architecture / MVP specification  
**Project:** Reusable, file-agnostic synchronization engine  
**Target platforms:** Desktop applications, initially macOS and Linux  
**Primary transport:** SSH

## 1. Project Overview

Clarra is a lightweight, decentralized file synchronization engine designed to be embedded in desktop applications.

The goal is to synchronize arbitrary application files across multiple computers without requiring a traditional database server, HTTP API, permanent server-side file storage, or application-specific data schemas.

A central server acts as a **coordination point**, maintaining a persistent ledger of files, revisions, participants, and outstanding transfers.

**The server is not the primary data repository.** Application data lives on participating computers. The server temporarily caches file content only when another participant needs it but cannot currently receive it.

The engine must support offline operation, automatic participant discovery, reliable synchronization, conflict preservation, and eventual consistency.

## 2. Core Principles

1. **File-agnostic:** Synchronize arbitrary files without understanding their formats or contents.
2. **SSH-based:** SSH handles authentication and communication. No HTTP service is required.
3. **Minimal setup:** Creating a project directory on the server and granting SSH/filesystem access is sufficient to establish a new project.
4. **Metadata-first:** The server maintains a persistent ledger but does not normally store file contents.
5. **Temporary caching:** File content is staged on the server only when required for outstanding deliveries.
6. **Offline-first:** Applications can read and modify local files without network connectivity.
7. **Conflict preservation:** Concurrent changes must never silently overwrite one another.
8. **Reusable engine:** Applications integrate the same synchronization engine without defining file schemas or writing custom sync logic.
9. **No mandatory P2P networking:** The initial version uses the SSH server as a relay. Direct P2P transfers may be introduced later.
10. **No content interpretation:** The engine operates on files, hashes, revisions, and byte streams—not business objects or application data types.

## 3. High-Level Architecture

The system consists of three components.

### A. Client synchronization engine

Runs locally alongside the application.

Responsibilities:
- Monitor the application's designated storage directory.
- Detect file creation, modification, deletion, and renaming.
- Maintain local revision history and synchronization state.
- Calculate file hashes and identify changes.
- Establish authenticated SSH connections.
- Exchange ledger metadata with the server.
- Upload and download required file content.
- Preserve conflicting file versions.
- Notify the application when conflicts require user intervention.
- Resume interrupted transfers.

### B. Server-side coordinator

A generic executable installed once on the server.

Responsibilities:
- Accept synchronization sessions over SSH.
- Verify directory-level authorization.
- Register participants and devices automatically.
- Maintain a persistent project ledger.
- Track which devices hold which file revisions.
- Track outstanding delivery requirements.
- Coordinate online transfers between connected participants.
- Request temporary uploads for offline participants.
- Manage the temporary cache and its cleanup.
- Recover its state after crashes or restarts.

The coordinator must not require per-project configuration or application-specific code.

### C. Project directory

Each synchronized project corresponds to a server directory.

Example:

    /projects/
        Clarra/
            .sync/
                ledger.log
                participants/
                index/
                cache/

All internal synchronization files are initialized automatically.

The ledger contains persistent metadata. The index contains derived metadata for efficient lookups. The cache contains temporary file content awaiting delivery.

**No application files are permanently stored in the project directory by default.**

## 4. Project Initialization and Joining

### First participant

1. Alice creates `/projects/Clarra` on the server and has permission to access it.
2. Alice launches the Clarra application on her Mac.
3. The application's sync engine connects to the server project over SSH.
4. If the ledger does not exist, the server initializes it automatically.
5. Alice's engine scans the application's local storage.
6. It creates a manifest containing file paths, sizes, hashes, and initial revision identifiers.
7. The server registers Alice's device and records the manifest.
8. No file contents are uploaded because there are no other participants requiring them.

The project is now initialized.

### Additional participants

1. Bob installs or launches the Clarra application.
2. Bob connects to the same server directory using his authorized SSH identity.
3. The server registers Bob's device as a participant.
4. Bob's engine scans its local storage and compares its state against the project ledger.
5. The server determines which file revisions Bob needs.
6. Those revisions are marked as pending deliveries.
7. The engine starts transferring available content or waits until a suitable source reconnects.

If Bob already has local files, they must not be blindly overwritten. Identical files can be recognized by hash; unrelated or concurrent versions of the same path must be preserved as conflicts.

### Participant identities

Each device must have a stable identity, associated with its authenticated SSH identity.

The ledger distinguishes between a user and individual devices when multiple devices belong to one user.

Project membership is determined by server-side SSH and filesystem access permissions. No separate application login is required.

Access revocation prevents future project synchronization, although it cannot retract files already received.

## 5. The Persistent Ledger

The ledger is the central coordination mechanism and must survive server restarts.

It is append-only or implemented with equivalent transactional durability guarantees.

The ledger tracks:

| Metadata | Purpose |
|---|---|
| Project ID | Unique project identity |
| Participant ID | Authorized user identity |
| Device ID | Unique replica identity |
| File path | Location within the application storage tree |
| Revision ID | Unique immutable revision |
| Content hash | Identifies and verifies file contents |
| File size | Transfer planning and validation |
| Parent revision IDs | Establishes revision ancestry and detects conflicts |
| Operation | Create, update, delete, or related metadata change |
| Content holders | Devices known to retain the revision |
| Pending recipients | Devices still requiring the revision |
| Cache availability | Whether the server currently holds the content |
| Delivery acknowledgments | Durable confirmation of successful receipt |

The ledger must support multiple concurrent revision heads for the same path.

A revision ID must identify a specific change, not merely a filename or modification timestamp. Content hashes may be shared by multiple revisions containing identical bytes.

### Suggested ledger events

- `PARTICIPANT_JOINED`
- `PARTICIPANT_REMOVED`
- `FILE_CREATED`
- `FILE_UPDATED`
- `FILE_DELETED`
- `CONTENT_AVAILABLE`
- `DELIVERY_REQUESTED`
- `DELIVERY_ACKNOWLEDGED`
- `CONTENT_CACHED`
- `CACHE_REMOVED`
- `CONFLICT_RESOLVED`

Event processing must be idempotent so retries do not create duplicate changes.

The implementation may use an append-only binary log with a rebuildable index. A separate database service is not required.

## 6. Synchronization Algorithm

### Step 1 — Local change detection

The client monitors local files using operating-system filesystem notifications, supplemented by periodic full scans.

When a file changes:

1. Wait until the write is stable enough to capture a consistent version.
2. Calculate its content hash.
3. Compare it with the last known revision.
4. If changed, create a new immutable revision referencing the previously observed parent revision(s).
5. Retain the content locally while required for synchronization.
6. Announce the revision to the server.

The application remains operational when offline. Changes are recorded locally until a connection is available.

### Step 2 — Metadata reconciliation

On connection or reconnection, the client identifies itself and supplies its latest acknowledged ledger position.

The server and client exchange missing ledger events or a compact equivalent state summary.

The engine determines:
- Which file revisions exist.
- Which devices hold each revision.
- Which revisions the client is missing.
- Whether any concurrent revisions conflict.
- Which transfers are required.

No file contents are transferred merely to perform this comparison.

### Step 3 — Transfer scheduling

For every required revision, the engine chooses an available source.

Preferred order:

1. Stream content from an online holder through the SSH coordinator to the online recipient.
2. If the recipient is offline, stage content in the server cache while a holder is online.
3. If the content is already cached, deliver it from the cache.
4. If no holder is online and the content is not cached, leave the request pending.

All transfer requests are tracked until fulfilled, canceled, or superseded safely.

### Step 4 — Receipt and cleanup

After receiving content, a client must:

1. Complete the transfer into a temporary local file.
2. Verify the expected size and cryptographic hash.
3. Persist the file durably.
4. Install it into local storage atomically where possible.
5. Record the revision as locally available.
6. Send a durable delivery acknowledgment.

Only after the server records this acknowledgment may it remove that device's outstanding delivery requirement.

Cached content may be deleted when no enrolled recipient still requires that content version.

## 7. Temporary Server Cache

The server cache is demand-driven.

**The server must not continuously mirror all application files.**

### Scenario A — Both participants online

Alice creates or updates a file.

Bob is online and requires the revision.

Alice streams the file to Bob through their existing SSH connections to the coordinator.

The server forwards the stream without permanently storing the content.

### Scenario B — Recipient offline

Alice creates or updates a file.

Bob is offline.

The ledger records that Bob needs the revision.

If Alice is online, the server requests an upload into its temporary cache. If Alice is also offline, the request remains pending until a holder reconnects.

When Bob reconnects, he downloads the content and acknowledges durable receipt.

The server removes the cached content when no remaining recipient requires it.

### Scenario C — Source offline

Alice possesses a file, but is offline.

Bob joins the project.

The server registers Bob's requirement without downloading anything.

When Alice reconnects, the coordinator schedules delivery or temporary caching.

### Scenario D — Multiple participants

When multiple recipients need identical content, one cached copy should satisfy all recipients.

The content remains cached until all required delivery acknowledgments are recorded, or the outstanding requirements are otherwise legitimately removed.

### Cache policies

- Content should be stored by cryptographic hash to enable deduplication.
- Partial uploads must not be advertised as complete.
- Interrupted uploads and downloads should be resumable.
- Cache deletion must be safe across crashes.
- Storage quotas and capacity errors must be supported.
- Expired or revoked participants must have an explicit policy for clearing outstanding requirements.
- Superseded revisions may be skipped only when doing so preserves correctness and conflict history.

**Important limitation:** The server cannot guarantee delivery while all content holders are permanently unavailable. Likewise, finite cache capacity can prevent immediate staging. In these cases the ledger preserves pending requirements until a source becomes available.

The cache is a delivery buffer, not a backup system.

## 8. Conflict Detection and Resolution

The engine must not attempt to interpret, parse, or merge arbitrary file contents.

Conflict detection operates on revision ancestry.

### Conflict example

Alice and Bob both possess revision `v1` of `plan.md`.

Alice modifies the file offline, producing `v2-A` with parent `v1`.

Bob independently modifies the same file, producing `v2-B` with parent `v1`.

Neither revision descends from the other.

The server records both revisions as concurrent heads and marks the path as conflicted.

### Required behavior

- Preserve both complete versions.
- Never silently select one revision and destroy the other.
- Provide deterministic conflict identifiers.
- Make both copies accessible to the application, including an optional conflict filename.
- Notify the application through a generic conflict API.
- Allow the user to choose one version or create a manually merged version.
- Register the resolution as a new revision referencing all resolved conflict heads.

Example:

    plan.md
    plan.conflict-bob-v2.md

Conflict copies generated by the engine must not accidentally be treated as independent new files and repeatedly synchronized.

The same rules apply to concurrent creation of the same path and deletion-versus-edit conflicts.

A deletion is represented by a tombstone revision, not by silently removing the file from the ledger.

The engine does not need to understand Markdown, JSON, images, documents, or any other content type.

## 9. SSH Communication Protocol

The MVP uses SSH exclusively as its externally accessible transport.

A client opens a bidirectional SSH session to a generic server-side executable.

Conceptually:

    ssh user@server clarra attach /projects/Clarra

The server-side implementation may use a globally installed coordinator and internal Unix-domain sockets to manage concurrent sessions. No per-project listening port is necessary.

### Protocol message types

| Message | Purpose |
|---|---|
| `HELLO` | Identify client, protocol version, and project |
| `STATE` | Exchange known ledger position and relevant revision state |
| `ANNOUNCE` | Publish a new revision |
| `WANT` | Request missing content |
| `OFFER` | Advertise content availability |
| `TRANSFER` | Send file content chunks |
| `ACK` | Confirm verified durable receipt |
| `PRESENCE` | Indicate online availability |
| `CANCEL` | Cancel an obsolete transfer requirement |
| `ERROR` | Report recoverable or permanent failures |

Messages should use a versioned, length-prefixed binary framing format.

File content is transferred as opaque bytes. The coordinator may inspect framing, identifiers, hashes, and transfer metadata, but must not inspect application-specific contents to determine synchronization behavior.

Transfers should support:
- Streaming without loading whole files into memory.
- Chunked transmission.
- Transfer IDs and offsets.
- Resume after interruption.
- Hash verification.
- Retry and idempotence.
- Flow control and bounded memory usage.

The transport interface should remain abstract enough to support direct P2P transfers later without changing revision or ledger semantics.

## 10. Client-Side Storage and Integration

The engine must be reusable across applications.

An application provides:
- A local storage directory.
- A remote project address.
- Access to an SSH identity.

No data type registration or content schema is required.

The engine maintains its own local metadata and retained revisions, ideally outside the application's ordinary synchronized files.

### Suggested generic API

    connect(local_directory, remote_project)
    start()
    pause()
    resume()
    status()
    list_conflicts()
    resolve_conflict(path, selected_content)
    disconnect()

These are conceptual interfaces, not prescribed function signatures.

Applications should be notified about:
- Synchronization progress.
- Newly received files.
- Transfer failures.
- Detected conflicts.
- Connection status.
- Storage or cache capacity problems.

The application remains responsible for displaying conflicts and collecting user resolution decisions.

## 11. Filesystem Rules

The MVP should explicitly define portable behavior for:

- Regular files and directories.
- Empty directories.
- File creation, updates, and deletion.
- File renames and moves.
- Files modified while transfers are in progress.
- Atomic file replacement by editors.
- Concurrent changes to the same path.
- Case-sensitive versus case-insensitive filenames.
- Unicode filename normalization.
- Executable permissions and other supported metadata.
- Symbolic links and filesystem traversal safety.
- Very large files and large directory trees.

Recommended MVP scope: regular files and directories, preserving relative paths and basic required metadata. Renames may initially be represented as deletion plus creation. Symbolic links should be excluded until a deliberate safe policy is implemented.

Engine-generated metadata, temporary files, and conflict copies must not trigger recursive synchronization loops.

File timestamps alone must never determine conflict winners.

## 12. Reliability and Consistency Guarantees

The intended consistency model is **eventual convergence**, assuming required participants and file holders eventually reconnect and transfers can complete.

Required guarantees:

1. No acknowledged revision is silently lost due to synchronization.
2. Concurrent revisions are preserved as separate heads until resolved.
3. Duplicate transfers or ledger messages have no harmful effect.
4. Missing content remains identifiable even when no holder is online.
5. Server restarts do not erase the ledger or valid cached transfers.
6. Interrupted transfers never produce valid-looking partial files.
7. Cached content is not deleted before all required durable acknowledgments.
8. Client reconnection can recover from missed events or interrupted sessions.
9. Ledger writes are serialized or transactionally coordinated across multiple connected clients.
10. A client must retain promised content locally until it is safely delivered, superseded without loss of correctness, or otherwise released.

The server must validate ledger events and safely recover any partially completed internal operations.

Ledger compaction is allowed, but must retain sufficient metadata for offline devices, conflict detection, and deletion history.

## 13. Security Model

Authentication is based on SSH.

Authorization is based on project-directory access, enforced by the server-side sync coordinator using operating-system identities and permissions or ACLs.

Recommended safeguards:

- Bind each registered device to an authenticated SSH identity.
- Validate access to the requested project directory on every session.
- Prevent directory traversal outside authorized project roots.
- Restrict SSH sessions to approved synchronization operations where practical.
- Protect coordinator-managed ledger and cache files from direct modification.
- Verify transferred content using cryptographic hashes.
- Validate message sizes, paths, revision references, and transfer limits.
- Prevent a participant from impersonating a different registered device.
- Support explicit participant removal and access revocation.

SSH encrypts data in transit. The server can still access temporarily cached file contents unless additional client-side encryption is introduced.

End-to-end encryption and encrypted ledger metadata are optional future features, not assumptions of the MVP.

## 14. Operational Requirements

### Server installation

The sync engine is installed once.

Creating new projects requires only:
1. Creating a directory.
2. Assigning appropriate SSH/filesystem access.
3. Connecting a client to that directory.

Project metadata must be initialized automatically.

No HTTP endpoint, project-specific database, custom schema, or separate project configuration is required.

The coordinator must provide basic diagnostics for participant state, pending transfers, cache occupancy, conflicts, and errors.

### Client operation

The client engine should run while the application is active or, optionally, as a desktop background agent.

It should reconnect automatically after sleep, network changes, and transient SSH failures.

The design must not assume all participants are online simultaneously.

## 15. MVP Implementation Milestones

### Phase 1 — Project registration and ledger

- Establish SSH connection and authorization.
- Automatically initialize a project.
- Register devices.
- Scan local directories.
- Publish manifests and immutable revision events.
- Maintain durable metadata without uploading file content.

### Phase 2 — Online synchronization

- Detect local changes.
- Exchange missing revisions.
- Transfer content between two online clients via SSH relay.
- Verify and acknowledge transfers.
- Apply incoming changes atomically.

### Phase 3 — Offline delivery and cache

- Track pending requirements for disconnected devices.
- Stage content when a holder becomes available.
- Deliver cached content when recipients reconnect.
- Delete cache objects after all required acknowledgments.
- Handle disconnects, retries, and partial transfers.

### Phase 4 — Conflict safety

- Detect concurrent file revisions.
- Preserve both versions.
- Handle deletion-versus-edit conflicts.
- Provide generic conflict notifications.
- Support explicit user resolution.
- Prevent repeated creation of conflict copies.

### Phase 5 — Production hardening

- Crash recovery.
- Concurrent-session coordination.
- Cache quotas.
- Ledger compaction.
- Resumable large-file transfers.
- Access revocation.
- Filesystem edge cases.
- Performance testing and diagnostics.

## 16. Minimum Acceptance Tests

The MVP is considered functional when all of the following pass.

| Test | Expected result |
|---|---|
| Alice initializes Clarra with existing files | Ledger created; server stores metadata but no file content |
| Bob joins while Alice is online | Bob receives all required files through the server relay |
| Bob joins while Alice is offline | Bob is registered; required files remain pending |
| Alice reconnects while Bob is offline | Required content is staged in the server cache |
| Bob later reconnects | Files transfer and verify; cache is deleted after durable acknowledgment |
| Alice and Bob edit different files offline | Both sets of edits synchronize successfully |
| Alice and Bob edit the same file offline | Both revisions survive and a conflict is reported |
| Alice deletes a file while Bob edits it | Neither operation silently destroys the other revision |
| A transfer is interrupted | Transfer resumes or retries safely without corrupting local files |
| The server restarts mid-synchronization | Ledger and valid cached data recover consistently |
| An unauthorized SSH identity attempts access | Project access is denied |
| Three or more participants need the same file | One cached content object can satisfy all outstanding recipients |
| Every participant acknowledges receipt | Server removes the unneeded cached content |

## 17. Explicit Non-Goals for the MVP

The first version will not implement:

- HTTP APIs or a web interface.
- A permanent centralized application-data store.
- Automatic text or semantic merging.
- Application-specific data schemas.
- Real-time collaborative document editing.
- Direct peer-to-peer NAT traversal.
- Mobile-specific background synchronization.
- Automatic end-to-end encryption.
- Multi-region server replication.
- Guaranteed backup or disaster recovery.

These features should not be allowed to complicate the initial architecture.

## 18. Decisions Required Before Implementation

The development team should resolve the following details:

1. **Device identity:** How to bind stable device IDs securely to SSH authentication.
2. **Cache capacity:** Maximum server storage and behavior when capacity is exhausted.
3. **Participant retirement:** How long to retain pending requirements for devices that never return, and how they are explicitly removed.
4. **Conflict materialization:** Exact filenames, application callbacks, and resolution workflow.
5. **File retention:** How clients preserve announced revisions even after local files are overwritten.
6. **Initial filesystem scope:** Symlink policy, permissions, renames, and platform-specific filename behavior.
7. **Ledger persistence:** Append-only log format, transactional locking, crash recovery, and safe compaction.
8. **Transfer format:** Chunk size, hashing algorithm, resumability, and protocol versioning.
9. **Security boundaries:** Whether clients have ordinary shell access or restricted synchronization-only SSH sessions.
10. **Availability policy:** How the system reports content that can no longer be retrieved from any known holder.

These are implementation and policy decisions; they do not require application-specific file type configuration.

## 19. Long-Term Extensions

The architecture should allow future development of:

- Direct encrypted P2P transfers.
- Real-time notifications of file changes.
- Collaborative editing through CRDTs or application-specific merge engines.
- Optional encrypted server caches.
- Selective synchronization of subdirectories.
- Multiple servers participating in one project.
- Bandwidth-efficient block-delta transfers.
- Advanced version history and restoration.

These enhancements should build on the same concepts of immutable revisions, content holders, pending requirements, and durable acknowledgments.

## Final Design Requirement

**Clarra must separate knowledge of the data from possession of the data.**

The server maintains authoritative synchronization metadata: which revisions exist, which participants possess them, and which transfers remain outstanding.

Participating computers maintain the actual application files.

The server may temporarily possess file content when needed to deliver it to an offline participant, but must remove that content after all corresponding delivery obligations have been fulfilled.

No participant's concurrent changes may be silently destroyed, and no application-specific knowledge of file formats may be required for the engine to operate.
