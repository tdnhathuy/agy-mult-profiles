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
    /// Manage profile aliases
    Alias {
        #[command(subcommand)]
        command: Option<AliasCommands>,
    },
    /// Run a prompt using a profile
    #[command(alias = "p", alias = "-p")]
    Prompt {
        /// Profile name or alias
        profile: Option<String>,
        /// Prompt text to send (reads from stdin if omitted)
        prompt: Option<String>,
        /// Model override for agy (e.g. gemini-2.5-flash)
        #[arg(long)]
        model: Option<String>,
        /// Directory to attach to agy context
        #[arg(long = "add-dir")]
        add_dir: Vec<String>,
        /// Do not automatically skip permissions (which is enabled by default)
        #[arg(long)]
        no_skip_perm: bool,
        /// Output result in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Execute arbitrary agy commands under a profile
    Exec {
        /// Profile name or alias
        profile: String,
        /// Raw arguments to pass to agy (use -- before flags)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show version or ping
    Ping,
}

#[derive(Subcommand)]
enum AliasCommands {
    /// Set an alias for a profile
    Set {
        /// Alias name
        alias: String,
        /// Target profile name
        profile: String,
    },
    /// Remove an alias
    Rm {
        /// Alias name
        alias: String,
    },
    /// List all aliases
    #[command(alias = "ls")]
    List {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
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
        Some(Commands::Alias { command }) => match command {
            Some(AliasCommands::Set { alias, profile }) => {
                commands::alias::cmd_alias_set(alias, profile)
            }
            Some(AliasCommands::Rm { alias }) => commands::alias::cmd_alias_rm(alias),
            Some(AliasCommands::List { json }) => commands::alias::cmd_alias_list(*json),
            None => commands::alias::cmd_alias_list(false),
        },
        Some(Commands::Prompt {
            profile,
            prompt,
            model,
            add_dir,
            no_skip_perm,
            json,
        }) => commands::prompt::cmd_prompt(
            profile.as_deref(),
            prompt.as_deref(),
            model.as_deref(),
            add_dir,
            *no_skip_perm,
            *json,
        ),
        Some(Commands::Exec { profile, args }) => commands::exec::cmd_exec(profile, args),
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
