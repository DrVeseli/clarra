use clap::{Parser, Subcommand};

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
    /// Show project diagnostics on the server.
    Status { project: std::path::PathBuf },
}

fn main() {
    let _ = Cli::parse();
}
