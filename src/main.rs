mod agy;
mod commands;
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
    /// Log in to an Antigravity profile or import existing credentials
    Login {
        /// Name of the profile
        name: String,
        /// Import existing ~/.gemini credentials
        #[arg(long)]
        import_current: bool,
    },
    /// List all profiles
    #[command(alias = "ls")]
    List {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Get or set the default profile
    Default {
        /// Profile name to set as default (leave empty to view current)
        profile: Option<String>,
    },
    /// Remove a profile
    Rm {
        /// Name of the profile to remove
        name: String,
        /// Do not prompt for confirmation
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Show version or ping
    Ping,
}

fn main() {
    let cli = Cli::parse();
    let res = match &cli.command {
        Some(Commands::Login {
            name,
            import_current,
        }) => commands::profile::cmd_login(name, *import_current),
        Some(Commands::List { json }) => commands::profile::cmd_list(*json),
        Some(Commands::Default { profile }) => commands::profile::cmd_default(profile.as_deref()),
        Some(Commands::Rm { name, yes }) => commands::profile::cmd_rm(name, *yes),
        Some(Commands::Ping) => {
            println!("pong");
            Ok(())
        }
        None => {
            println!("Use 'agyp --help' for usage instructions.");
            Ok(())
        }
    };

    if let Err(code) = res {
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_scaffold_runs() {
        assert_eq!(2 + 2, 4);
    }
}
