# Green Sync

Green Sync is an MVP file synchronization engine and CLI. Devices keep the
working files; an SSH-accessible coordinator stores revision metadata and
temporarily caches content needed by another registered device.

## Install

Green Sync requires a Rust toolchain with Rust 2024 edition support and an
`ssh` client for remote synchronization.

```sh
cargo install --path .
```

Install the same `green-sync` binary on the SSH server and ensure it is on the
remote user's `PATH`. To build without installing:

```sh
cargo build --release
```

The binary is written to `target/release/green-sync`.

## Quick start

Create existing directories for the local working tree and server project.
The `local:` transport exercises the complete protocol without SSH:

```sh
mkdir -p /tmp/green-local /tmp/green-server
green-sync sync /tmp/green-local local:/tmp/green-server
green-sync sync /tmp/green-local local:/tmp/green-server --watch
```

For SSH, create a writable project directory on the server, then use a remote
in `[user@]host:/absolute/project/path` form:

```sh
ssh sync@example.com 'mkdir -p /srv/green-sync/team'
mkdir -p ./team
green-sync sync ./team sync@example.com:/srv/green-sync/team
green-sync sync ./team sync@example.com:/srv/green-sync/team --watch
```

Device identity and registration are automatic on the first sync. The local
directory must already exist. See the deployment guide before exposing a
project to multiple SSH users.

## Documentation

- [Architecture, storage, recovery, and MVP limits](docs/architecture.md)
- [Server installation and security boundaries](docs/deployment.md)
- [CLI operations, status, cache quota, and protocol](docs/protocol-and-operations.md)
- [Rust and Swift integration examples](docs/integration.md)
- [Product specification](product.md)

## Development

```sh
cargo test
cargo run -- --help
```
