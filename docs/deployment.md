# Server Deployment and Security

## Installation

Build or install the same protocol-compatible binary on every client and the
server:

```sh
cargo build --release
cargo install --path .
```

On the server, place `clarra` on the SSH user's non-interactive `PATH` and
create a project directory writable by that user:

```sh
sudo install -m 0755 target/release/clarra /usr/local/bin/clarra
sudo mkdir -p /srv/clarra/team
sudo chown sync-user:sync-user /srv/clarra/team
```

The project directory must exist before the first session. The first `attach`,
`status`, or local test session creates `<project>/.sync/` and initializes its
project ID.

Test command availability through the same SSH identity used by clients:

```sh
ssh sync-user@example.com 'command -v clarra'
ssh sync-user@example.com 'clarra status /srv/clarra/team'
```

Clients use `[user@]host:/project/path`:

```sh
clarra sync ./team sync-user@example.com:/srv/clarra/team --watch
```

Internally this launches the equivalent of:

```sh
ssh -- sync-user@example.com "clarra attach -- '/srv/clarra/team'"
```

`attach` is a binary stdin/stdout protocol endpoint, not an interactive shell
command. SSH host keys, keys/agents, jump hosts, and other connection policy
come from the system `ssh` client and its configuration.

## Automatic registration

The local engine creates a device ID and token automatically. On the first
successful `HELLO`, an unknown device is registered in the project ledger and
bound to the server process's effective Unix user ID. No separate enrollment
command is required.

Later sessions using that device ID must present the same token and run under
the same effective user ID. Reusing an ID with a different token or effective
user is rejected. Different SSH accounts mapped to the same Unix UID are the
same participant from Clarra's perspective.

## Authorization boundaries

Clarra relies on deployment controls for project membership:

- SSH decides who may reach the server and execute a command.
- Unix filesystem permissions decide which project directories that process
  can open and modify.
- The stored device token prevents an existing device ID from being taken over
  by another token, but it is not initial admission control.
- Any caller that can execute `attach` as an authorized Unix user and access a
  project can register a new device automatically.

Within an authenticated session, the server requires `HELLO` first, checks the
protocol version, limits announcements to 1,000 revisions, and requires each
announced revision's author to match the session device. Uploads are accepted
only from a recorded holder for content currently needed by a recipient.
Downloads are allowed only when that content is pending for the requesting
device. Hashes, sizes, portable paths, chunk sizes, and final content hashes
are validated.

The client rejects symlink traversal in its metadata and destination path
components. The server rejects `.sync`, cache, or index when those paths are
symlinks. These checks do not replace restrictive ownership and permissions on
the project directory.

For a restricted deployment, configure SSH outside Clarra so the account
can run only approved commands and access only intended project directories.
The repository does not provide an `authorized_keys` forced-command wrapper,
path allowlist, sandbox, or privilege separation. Do not expose arbitrary
project paths to an untrusted shell account.
