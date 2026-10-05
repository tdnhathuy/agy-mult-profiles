mod agy;
mod commands;
mod config;
mod isolate;
mod isolated_probe;
pub mod keychain;
mod security;

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
enum IsolateCommands {
    Bind {
        profile: String,
        #[arg(long)]
        user: String,
    },
    Unbind {
        profile: String,
    },
    Status {
        profile: Option<String>,
    },
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
        /// Auto-approve AGY tool permissions (use only in an isolated worktree)
        #[arg(long)]
        skip_permissions: bool,
        /// Output result in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Run an autonomous task on an isolated Git worktree
    #[command(version)]
    Task {
        /// Name of the profile (mandatory, explicit)
        #[arg(long)]
        profile: String,
        /// Path to the isolated Git linked worktree
        #[arg(long)]
        worktree: std::path::PathBuf,
        /// Path to prompt file (reads from stdin if omitted)
        #[arg(long)]
        prompt_file: Option<std::path::PathBuf>,
        /// Timeout duration (e.g. 20m, 30s, 1h; default: 20m)
        #[arg(long)]
        timeout: Option<String>,
        /// Model override for agy (e.g. gemini-2.5-flash)
        #[arg(long)]
        model: Option<String>,
        /// Auto-approve AGY tool permissions (valid only with worktree)
        #[arg(long)]
        skip_permissions: bool,
        /// Output result in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Create an empty private sandbox for manual SSH OAuth investigation (does not invoke agy)
    IsolatedProbe {
        /// Explicit profile name (not an alias)
        profile: String,
    },
    /// Bind a profile to a separate logged-in macOS local user
    Isolate {
        #[command(subcommand)]
        command: IsolateCommands,
    },
    /// Execute arbitrary agy commands under a profile
    Exec {
        /// Profile name or alias
        profile: String,
        /// Raw arguments to pass to agy (use -- before flags)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Check quota status for profiles
    Quota {
        /// Name of the profile or alias
        #[arg(long)]
        profile: Option<String>,
        /// Check all registered profiles
        #[arg(long)]
        all: bool,
        /// Timeout duration (e.g. 30s, 1m; default: 30s)
        #[arg(long)]
        timeout: Option<String>,
        /// Bypass cache and force fresh probe
        #[arg(long)]
        no_cache: bool,
        /// Output result in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Show detailed Gemini quota for one profile
    #[command(alias = "q")]
    Usage {
        /// Name of one profile or alias (omit to check all profiles)
        profile: Option<String>,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
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

fn handle_task_arg_error(args: &[String], err: &clap::Error) -> ! {
    let mut profile = String::new();
    let mut worktree = String::new();
    let mut timeout_err = false;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--profile" && i + 1 < args.len() {
            profile = args[i + 1].clone();
            i += 1;
        } else if let Some(p) = args[i].strip_prefix("--profile=") {
            profile = p.to_string();
        } else if args[i] == "--worktree" && i + 1 < args.len() {
            worktree = args[i + 1].clone();
            i += 1;
        } else if let Some(w) = args[i].strip_prefix("--worktree=") {
            worktree = w.to_string();
        } else if args[i] == "--timeout" && i + 1 < args.len() {
            let t = &args[i + 1];
            if crate::commands::task::parse_duration(t).is_err() {
                timeout_err = true;
            }
            i += 1;
        } else if let Some(t) = args[i].strip_prefix("--timeout=") {
            if crate::commands::task::parse_duration(t).is_err() {
                timeout_err = true;
            }
        }
        i += 1;
    }

    let failure_kind = if timeout_err {
        crate::commands::task::TaskFailureKind::InvalidDuration
    } else if profile.is_empty() {
        crate::commands::task::TaskFailureKind::InvalidProfile
    } else if worktree.is_empty() {
        crate::commands::task::TaskFailureKind::InvalidWorktree
    } else {
        crate::commands::task::TaskFailureKind::SetupError
    };

    let err_str = err.to_string();
    let json_out = crate::commands::task::TaskJsonOutput {
        profile,
        worktree,
        canonical_worktree: None,
        status: "error".to_string(),
        failure_kind: Some(failure_kind.as_str().to_string()),
        agent_failure_kind: None,
        restore_error: None,
        exit_code: None,
        agent_exit_code: None,
        cli_exit_code: 1,
        timed_out: false,
        duration_ms: 0,
        stdout: String::new(),
        stderr: err_str,
    };

    println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
    std::process::exit(1);
}

fn handle_quota_arg_error(args: &[String], err: &clap::Error) -> ! {
    let mut profile = "unknown".to_string();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--profile" && i + 1 < args.len() && !args[i + 1].starts_with('-') {
            profile = args[i + 1].clone();
            i += 1;
        } else if let Some(p) = args[i].strip_prefix("--profile=") {
            if !p.is_empty() {
                profile = p.to_string();
            }
        }
        i += 1;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let result = serde_json::json!({
        "profile": profile,
        "email": null,
        "status": "error",
        "observed_at": now,
        "cached": false,
        "source": "preflight",
        "detail": err.to_string().trim(),
        "agent_exit_code": null,
    });
    let json_out = serde_json::json!({
        "schema_version": 1,
        "checked_at": now,
        "source": "preflight",
        "results": [result],
        "cli_exit_code": 1,
    });

    println!("{}", serde_json::to_string_pretty(&json_out).unwrap());
    std::process::exit(1);
}

fn get_first_subcommand(raw_args: &[String]) -> Option<&str> {
    let mut i = 1;
    while i < raw_args.len() {
        let arg = &raw_args[i];
        if arg == "--" {
            return raw_args.get(i + 1).map(|s| s.as_str());
        }
        if !arg.starts_with('-') {
            return Some(arg.as_str());
        }
        i += 1;
    }
    None
}

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse_from(&raw_args) {
        Ok(c) => c,
        Err(err) => {
            // Finding 5: Preserve normal Clap success semantics for --help and --version
            if err.kind() == clap::error::ErrorKind::DisplayHelp
                || err.kind() == clap::error::ErrorKind::DisplayVersion
            {
                err.exit();
            }

            let first_sub = get_first_subcommand(&raw_args);
            let is_task_subcommand = first_sub == Some("task");
            let is_quota_subcommand = first_sub == Some("quota");
            if is_task_subcommand && raw_args.iter().any(|a| a == "--json") {
                handle_task_arg_error(&raw_args, &err);
            } else if is_quota_subcommand && raw_args.iter().any(|a| a == "--json") {
                handle_quota_arg_error(&raw_args, &err);
            } else {
                err.exit();
            }
        }
    };
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
            skip_permissions,
            json,
        }) => commands::prompt::cmd_prompt(
            profile.as_deref(),
            prompt.as_deref(),
            model.as_deref(),
            add_dir,
            *skip_permissions,
            *json,
        ),
        Some(Commands::Task {
            profile,
            worktree,
            prompt_file,
            timeout,
            model,
            skip_permissions,
            json,
        }) => commands::task::cmd_task(
            profile,
            worktree,
            prompt_file.as_deref(),
            timeout.as_deref(),
            model.as_deref(),
            *skip_permissions,
            *json,
        ),
        Some(Commands::Quota {
            profile,
            all,
            timeout,
            no_cache,
            json,
        }) => commands::quota::cmd_quota(
            profile.as_deref(),
            *all,
            timeout.as_deref(),
            *no_cache,
            *json,
        ),
        Some(Commands::IsolatedProbe { profile }) => {
            isolated_probe::prepare(profile).map_err(|e| {
                eprintln!("Isolated probe unavailable: {e}");
                1
            })
        }
        Some(Commands::Isolate { command }) => {
            let action = match command {
                IsolateCommands::Bind { profile, user } => isolate::bind(profile, user),
                IsolateCommands::Unbind { profile } => isolate::unbind(profile),
                IsolateCommands::Status { profile } => isolate::status(profile.as_deref()),
            };
            action.map_err(|e| {
                eprintln!("Isolate: {e}");
                1
            })
        }
        Some(Commands::Exec { profile, args }) => commands::exec::cmd_exec(profile, args),
        Some(Commands::Usage { profile, json }) => {
            commands::usage::cmd_usage(profile.as_deref(), *json)
        }
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
