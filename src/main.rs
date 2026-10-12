use clap::{Parser, Subcommand};
use green_sync::Engine;

#[derive(Parser)]
#[command(name = "green-sync", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve a synchronization session over stdin/stdout.
    Attach { project: std::path::PathBuf },
    /// Synchronize a local directory with an SSH project.
    Sync {
        local: std::path::PathBuf,
        remote: String,
        #[arg(long)]
        watch: bool,
    },
    /// Resolve all conflict heads for a path with selected content.
    Resolve {
        local: std::path::PathBuf,
        remote: String,
        path: String,
        content: std::path::PathBuf,
    },
    /// Show project diagnostics on the server.
    Status { project: std::path::PathBuf },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Attach { project } => {
            green_sync::server::serve(&project, std::io::stdin().lock(), std::io::stdout().lock())
        }
        Command::Sync {
            local,
            remote,
            watch,
        } => {
            let engine = Engine::new(local, remote);
            if watch {
                engine.watch(print_event)
            } else {
                for event in engine.sync_once()? {
                    print_event(event);
                }
                Ok(())
            }
        }
        Command::Resolve {
            local,
            remote,
            path,
            content,
        } => {
            let engine = Engine::new(local, remote);
            engine.resolve_conflict(&path, &std::fs::read(content)?)?;
            for event in engine.sync_once()? {
                print_event(event);
            }
            Ok(())
        }
        Command::Status { project } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&green_sync::server::diagnostics(&project)?)?
            );
            Ok(())
        }
    }
}

fn print_event(event: green_sync::EngineEvent) {
    println!(
        "{}",
        serde_json::to_string(&event).expect("event serializes")
    );
}
