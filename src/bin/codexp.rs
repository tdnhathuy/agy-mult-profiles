use std::path::PathBuf;

use agyp::codex::commands;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "codexp",
    version,
    about = "Codex CLI multi-profile runner for imported ChatGPT accounts"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Import (or refresh) a profile from an exported Codex auth.json
    Import {
        /// Name of the profile
        name: String,
        /// Exported auth.json file (e.g. from OmniRoute)
        #[arg(long)]
        from: PathBuf,
        /// Allow replacing the profile's account or importing an account twice
        #[arg(long)]
        force: bool,
        /// Delete the export file after a successful import
        #[arg(long)]
        remove_source: bool,
    },
    /// List profiles and token validity
    #[command(alias = "ls")]
    List {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Remove a profile and its CODEX_HOME
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
    /// Run `codex exec` for a prompt in an isolated Git linked worktree
    Task {
        /// Profile name or alias (mandatory, explicit)
        #[arg(long)]
        profile: String,
        /// Path to the isolated Git linked worktree
        #[arg(long)]
        worktree: PathBuf,
        /// Path to prompt file (reads from stdin if omitted)
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// Timeout duration (e.g. 20m, 30s, 1h; default: 20m)
        #[arg(long)]
        timeout: Option<String>,
        /// Model override (e.g. gpt-5.5)
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort override (e.g. low, medium, high)
        #[arg(long)]
        reasoning_effort: Option<String>,
        /// Sandbox: read-only, workspace-write or danger-full-access
        #[arg(long, default_value = "workspace-write")]
        sandbox: String,
        /// Continue an earlier task's session (thread_id from its output)
        #[arg(long)]
        resume: Option<String>,
        /// Output result in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Show ChatGPT Codex quota (5-hour and weekly windows) for one or all profiles
    #[command(alias = "q")]
    Quota {
        /// Profile name or alias (omit for all profiles)
        profile: Option<String>,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Log plain `codex` (~/.codex) in as a profile, or show which profile it uses
    Active {
        /// Profile name or alias (omit to show the active profile)
        profile: Option<String>,
    },
    /// Run any codex command under a profile (no args = interactive TUI)
    Exec {
        /// Profile name or alias
        profile: String,
        /// Raw arguments for codex (use -- before flags)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AliasCommands {
    /// Set an alias for a profile
    Set { alias: String, profile: String },
    /// Remove an alias
    Rm { alias: String },
    /// List all aliases
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let res = match &cli.command {
        Some(Commands::Import {
            name,
            from,
            force,
            remove_source,
        }) => commands::cmd_import(name, from, *force, *remove_source),
        Some(Commands::List { json }) => commands::cmd_list(*json),
        Some(Commands::Rm { name, yes }) => commands::cmd_rm(name, *yes),
        Some(Commands::Alias { command }) => match command {
            Some(AliasCommands::Set { alias, profile }) => commands::cmd_alias_set(alias, profile),
            Some(AliasCommands::Rm { alias }) => commands::cmd_alias_rm(alias),
            Some(AliasCommands::List { json }) => commands::cmd_alias_list(*json),
            None => commands::cmd_alias_list(false),
        },
        Some(Commands::Task {
            profile,
            worktree,
            prompt_file,
            timeout,
            model,
            reasoning_effort,
            sandbox,
            resume,
            json,
        }) => commands::cmd_task(
            &commands::TaskArgs {
                profile,
                worktree,
                prompt_file: prompt_file.as_deref(),
                timeout: timeout.as_deref(),
                model: model.as_deref(),
                reasoning_effort: reasoning_effort.as_deref(),
                sandbox,
                resume: resume.as_deref(),
            },
            *json,
        ),
        Some(Commands::Quota { profile, json }) => {
            agyp::codex::quota::cmd_quota(profile.as_deref(), *json)
        }
        Some(Commands::Active { profile }) => commands::cmd_active(profile.as_deref()),
        Some(Commands::Exec { profile, args }) => commands::cmd_exec(profile, args),
        None => {
            println!("Use 'codexp --help' for usage instructions.");
            Ok(())
        }
    };
    if let Err(code) = res {
        std::process::exit(code);
    }
}
