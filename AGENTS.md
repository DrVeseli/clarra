# Repository Guide

## Verification

- Match CI order: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, then `cargo test`. CI runs on both macOS and Linux.
- Run one unit test with `cargo test <module>::tests::<name>`; run one CLI integration test with `cargo test --test sync <name>`.
- `tests/sync.rs` exercises the full child-process protocol through `local:<project>` and needs no SSH server.
- Release builds use `cargo build --locked --release`; dependency changes must include `Cargo.lock`.

## Runtime Map

- `src/main.rs` is the CLI entrypoint: `sync`/`resolve` call `client::Engine`, `attach` runs the server protocol, and `status [--tui]` reads server diagnostics.
- `client::Session` launches the current executable as `attach` for `local:` remotes, or system `ssh` for `[user@]host:/absolute/path`; there is no daemon or Clarra network port.
- `ledger.log` is authoritative append-only JSONL. `.sync/index/state.json` is derived and never read as authoritative state.
- Server content under `.sync/cache` is a temporary delivery buffer, not a backup. Local retained objects under `.clarra/objects` are also not a backup.

## Correctness Boundaries

- Never print diagnostics or logs to stdout from `attach`; stdout is the length-prefixed Postcard protocol. Use stderr for diagnostics.
- Wire changes to `Request`, `Response`, or their serialized fields require reviewing and normally incrementing `PROTOCOL_VERSION`; deployed clients and servers must remain protocol-compatible.
- Every new ledger `Event` must be handled in `ProjectState::apply`; replay must remain deterministic, sequence-checked, and idempotent.
- Do not mutate or delete `.sync/ledger.log`, `.sync/cache`, or derived state to `repair` tests. `Ledger::open` only repairs an incomplete final ledger line.
- Opening `attach` or `status` initializes `<project>/.sync`; use a temporary existing directory when testing commands manually.
- Deleting `<local>/.clarra/state.json` creates a new device identity and loses local revision bookkeeping.
- The scanner intentionally excludes symlinks, `.clarra/`, generated `.clarra-*` names, and conflict copies; preserve these exclusions when changing traversal.
- Device membership is bound to the server process's effective Unix UID plus a local token. SSH key comments and live presence are not available to the dashboard.

## Useful References

- `docs/architecture.md`: sync sequence, storage layout, recovery rules, and explicit MVP limits.
- `docs/protocol-and-operations.md`: wire framing, cache quota, CLI output, and TUI behavior.
- `docs/deployment.md`: SSH command construction and authorization boundaries.
