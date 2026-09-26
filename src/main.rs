mod config;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agyp",
    version,
    about = "Antigravity Multi-Profile CLI Manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show version or ping
    Ping,
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::Ping) => println!("pong"),
        None => println!("Use 'agyp --help' for usage instructions."),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_scaffold_runs() {
        assert_eq!(2 + 2, 4);
    }
}
