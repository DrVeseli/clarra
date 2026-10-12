# Clarra

Clarra is an MVP file synchronization engine and CLI. Devices keep the
working files; an SSH-accessible coordinator stores revision metadata and
temporarily caches content needed by another registered device.

## Install

Clarra requires a Rust toolchain with Rust 2024 edition support and an
`ssh` client for remote synchronization.

Prebuilt macOS and Linux binaries are available from
[GitHub Releases](https://github.com/DrVeseli/clarra/releases). Each archive
includes a matching SHA-256 checksum.

```sh
cargo install --path .
```

Install the same `clarra` binary on the SSH server and ensure it is on the
remote user's `PATH`. To build without installing:

```sh
cargo build --release
```

The binary is written to `target/release/clarra`.

## Quick start

Create existing directories for the local working tree and server project.
The `local:` transport exercises the complete protocol without SSH:

```sh
mkdir -p /tmp/clarra-local /tmp/clarra-server
clarra sync /tmp/clarra-local local:/tmp/clarra-server
clarra sync /tmp/clarra-local local:/tmp/clarra-server --watch
```

For SSH, create a writable project directory on the server, then use a remote
in `[user@]host:/absolute/project/path` form:

```sh
ssh sync@example.com 'mkdir -p /srv/clarra/team'
mkdir -p ./team
clarra sync ./team sync@example.com:/srv/clarra/team
clarra sync ./team sync@example.com:/srv/clarra/team --watch
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
