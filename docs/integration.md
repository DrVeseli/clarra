# Integration Examples

## Rust

The package is named `green-sync`; its Rust crate name is `green_sync`. For a
checkout dependency:

```toml
[dependencies]
anyhow = "1"
green-sync = { path = "../green-sync" }
```

`Engine::new` only stores the local and remote values. The first `sync_once`
or `watch` call creates local identity state and automatically registers that
device with the server; there is no registration API to call first.

The crate root re-exports `Engine`, `EngineEvent`, `Conflict`, and `Revision`.
`Engine` provides `new`, `sync_once`, `watch`, and `resolve_conflict`; the
`client`, `ledger`, `model`, `protocol`, and `server` modules are also public.

```rust
use green_sync::{Engine, EngineEvent};

fn report(event: EngineEvent) {
    match event {
        EngineEvent::Connected => println!("connected"),
        EngineEvent::FileReceived { path } => println!("received {path}"),
        EngineEvent::FilePublished { path } => println!("published {path}"),
        EngineEvent::Conflict(conflict) => {
            println!("conflict {}: {:?}", conflict.path, conflict.revision_ids)
        }
        EngineEvent::Error { message } => eprintln!("sync error: {message}"),
    }
}

fn main() -> anyhow::Result<()> {
    let engine = Engine::new(
        "./team",
        "sync-user@example.com:/srv/green-sync/team",
    );

    for event in engine.sync_once()? {
        report(event);
    }

    // Blocks indefinitely under normal operation and delivers EngineEvent values.
    engine.watch(report)
}
```

`sync_once` returns `Vec<EngineEvent>` or an error. `watch` performs an initial
sync, then calls the callback for each event. Per-pass failures in watch mode
arrive as `EngineEvent::Error` instead of ending the loop.

## Swift

There is no native Swift API in this repository. Launch the CLI with
Foundation `Process` and decode each stdout line. Device registration remains
automatic because the CLI engine creates and stores identity under
`<local>/.green-sync/` during its first pass.

```swift
import Foundation

struct GreenSyncEvent: Decodable {
    let type: String
    let path: String?
    let revision_ids: [String]?
    let message: String?
}

func watchGreenSync(
    local: String,
    remote: String,
    onEvent: @escaping (GreenSyncEvent) -> Void
) throws -> Process {
    let process = Process()
    let stdout = Pipe()

    // `env` resolves green-sync through PATH.
    process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
    process.arguments = ["green-sync", "sync", local, remote, "--watch"]
    process.standardOutput = stdout
    process.standardError = FileHandle.standardError

    try process.run()

    DispatchQueue.global(qos: .utility).async {
        let handle = stdout.fileHandleForReading
        var buffer = Data()

        while process.isRunning {
            guard let chunk = try? handle.read(upToCount: 4096),
                  !chunk.isEmpty else { break }
            buffer.append(chunk)

            while let newline = buffer.firstIndex(of: 0x0A) {
                let line = Data(buffer[..<newline])
                buffer.removeSubrange(...newline)
                if let event = try? JSONDecoder().decode(GreenSyncEvent.self, from: line) {
                    onEvent(event)
                }
            }
        }
    }

    return process
}

let process = try watchGreenSync(
    local: "/Users/alice/Team",
    remote: "sync-user@example.com:/srv/green-sync/team"
) { event in
    print(event.type, event.path ?? event.message ?? "")
}

// Retain `process` for as long as synchronization should run.
```

The callback runs on the utility queue. A production UI should dispatch model
or view updates to its required executor, retain the returned `Process`, watch
termination status, and surface stderr separately from JSON notifications.
