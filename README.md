# agyp & codexp — multi-account CLIs for Antigravity and Codex

Run [Google Antigravity](https://antigravity.google/) (`agy`) and [OpenAI Codex CLI](https://github.com/openai/codex) (`codex`) under several accounts on one machine (macOS or Linux), with each account's credentials fully isolated.

- **`agyp`** manages multiple `agy` accounts. Every profile gets its own credential store (a private HOME and Keychain on macOS, a private token file on Linux), so logins never collide and profiles can run in parallel.
- **`codexp`** runs `codex` under multiple imported ChatGPT accounts, one `CODEX_HOME` per profile.

Both are built for **developers and AI agents** (Claude Code, Codex, Hermes, Orca, …) that need to delegate prompts or coding tasks to a specific account non-interactively and parse the result.

> **Unofficial.** This project is not affiliated with or endorsed by Google or OpenAI. You are responsible for following the terms of service of the accounts you use with it.

---

## Highlights

- **Real Keychain isolation (macOS).** `agy` reads its token from one fixed Keychain item, so normally only one account can be logged in at a time. `agyp` gives every profile a private HOME and Keychain under `~/.gemini-profiles/homes/<name>/`. The system Keychain is never touched.
- **Linux / Ubuntu Server support.** On Linux each profile keeps its token in its own `agy` token file, and `agyp` pins `agy` to that file so a desktop keyring is never shared between profiles. No keyring, D-Bus or extra packages are needed, so it works on headless servers, over SSH and from cron or systemd.
- **Parallel profiles.** Locks are per profile and held only for a few seconds, so different profiles (and even several runs of the same profile) can work at the same time.
- **Browser OAuth per profile.** `agyp login <name>` runs the OAuth flow against the profile's own credential store, so Google shows the account picker instead of reusing your current login.
- **Agent-friendly output.**
  - Clean stdout with no wrapper noise.
  - `--json` on the commands agents care about.
  - Stable exit codes: `0` success, `1` not found / invalid input, `2` unauthenticated, `127` `agy` binary missing.
  - `agyp task` / `codexp task` run in a Git linked worktree with a hard timeout, concurrent stream draining and a structured failure classification.
- **Safe defaults.** AGY permission prompts stay on unless you pass `--skip-permissions` (only allowed inside a worktree). Profiles are never rotated automatically on `429` / quota errors.
- **Quota at a glance.** `agyp q` and `codexp q` show the remaining Gemini / GPT / Codex quota for every profile.
- **One login switch, one history.** `codexp active <profile>` makes plain `codex` (and the IDE extension / Codex app) use a codexp profile, and plain `codex` and every profile share one session history in `~/.codex`, so any thread resumes under any account.
- **Aliases.** Map short handles such as `p1` or `work` to profiles.
- **Agent skills included.** `skills/agyp` and `skills/codexp` teach coding agents how to hand work to these CLIs.

---

## Platform support

| Platform | `agyp` | `codexp` | How `agyp` isolates accounts |
| :--- | :--- | :--- | :--- |
| macOS | ✅ | ✅ | Private HOME + private Keychain per profile |
| Linux (Ubuntu Server, headless / SSH) | ✅ | ✅ | Private `agy` token file per profile, no keyring needed |

Windows is not supported.

## Requirements

- macOS (Keychain isolation relies on `/usr/bin/security`) or Linux (developed against Ubuntu Server 24.04 and `agy` 1.3.4)
- [Rust](https://rustup.rs/) toolchain (to build)
- `agy` on your `PATH` for `agyp`
- `codex` on your `PATH` for `codexp`

## Installation

```bash
git clone https://github.com/tdnhathuy/agy-mult-profiles.git
cd agy-mult-profiles
make install          # builds release binaries and copies agyp + codexp to ~/.local/bin
```

Make sure `~/.local/bin` is on your `PATH`:

```bash
export PATH="$HOME/.local/bin:$PATH"
agyp --version
codexp --version
```

---

## agyp quick start

### 1. Add profiles

Import the account `agy` is already logged in with:

```bash
agyp login default --import-current
```

Add more accounts. Each command opens the Google OAuth page in your browser; pick the account you want for that profile. On a server without a browser, `agy` prints the login URL instead: open it on any machine and paste the authorization code back into the terminal.

```bash
agyp login profile-1
agyp login profile-2
```

The profile is registered as soon as OAuth finishes and is usable right away.

### 2. Set aliases (optional)

```bash
agyp alias set p1 profile-1
agyp alias set p2 profile-2
```

### 3. Check status

```bash
agyp list
```

```text
PROFILE         EMAIL                          ALIASES         DEFAULT    STATUS
------------------------------------------------------------------------------------------
default         user1@gmail.com                -               yes (*)    ready
profile-1       user2@gmail.com                p1              no         ready
profile-2       user3@gmail.com                p2              no         ready
```

### 4. Use it

```bash
agyp q                                   # remaining quota for every profile
agyp prompt p1 "Summarize this repo"     # one-off prompt
agyp exec p2                             # open the agy TUI as profile-2
agyp exec p2 -- --model gemini-2.5-flash -i "Fix the failing test"

# Autonomous coding task in an isolated Git linked worktree
git worktree add ../wt-fix -b fix/flaky-test
agyp task --profile p1 --worktree ../wt-fix --prompt-file prompt.md --timeout 30m --json
```

---

## agyp command reference

| Command | Description |
| :--- | :--- |
| `agyp login <name>` | Log in a new account via browser OAuth |
| `agyp login <name> --import-current` | Import the account `agy` is currently logged in with |
| `agyp list [--json]` | List profiles, emails, aliases and status |
| `agyp default [name]` | Show or change the default profile |
| `agyp rm <name> [-y]` | Remove a profile and its data |
| `agyp alias set <alias> <profile>` | Create an alias |
| `agyp alias rm <alias>` / `agyp alias list` | Remove / list aliases |
| `agyp prompt <profile> "<text>"` | Send a prompt (reads stdin if the text is omitted). Options: `--model`, `--add-dir`, `--skip-permissions`, `--json` |
| `agyp task --profile <p> --worktree <path>` | Run an autonomous task in a Git linked worktree. Options: `--prompt-file`, `--timeout` (default `20m`), `--model`, `--skip-permissions`, `--json` |
| `agyp exec <profile> [-- <args...>]` | Pass arbitrary arguments to `agy` under a profile |
| `agyp q [profile]` / `agyp usage [profile]` | Gemini and GPT quota: remaining %, usage bar, time until reset. `--json`, `--hide-gpt` |
| `agyp quota --profile <p>` / `--all` | Same quota view; with `--json` it runs a minimal health probe (cached 5 min, `--no-cache`, `--timeout`) |

Use `agyp task` for coding work. `agyp prompt` has no worktree isolation, deadline or failure classification, so keep it for non-coding prompts.

---

## How the isolation works

- `agy` reads its token from a single Keychain item (`service=gemini`, `account=antigravity`) via `/usr/bin/security`, and `security` picks the Keychain based on `$HOME`. So `agyp` gives each profile its own HOME at `~/.gemini-profiles/homes/<name>/`:
  - `Library/Keychains/agyp.keychain-db` is the profile's private Keychain (not `login.keychain-db`, which macOS 27.2+ will not unlock by password). Its random password is stored in `~/.gemini-profiles/profiles/<name>/keychain_password`, and it does not auto-lock.
  - Everything else from your real HOME (`.gitconfig`, `.ssh`, `.config`, `.gemini`, `Library/*`, …) is symlinked, so git, npm and other tools keep working inside tasks. Only `Library/Keychains` and `Library/Preferences` are private.
  - Before every run, `agyp` verifies that the Keychain search list and default Keychain under the private HOME point only at the profile's Keychain, and refuses to run otherwise.
- The private Keychain is the source of truth for the profile's token. The first run seeds it from `~/.gemini-profiles/profiles/<name>/keychain_token.json`; `agy` refreshes the token inside the private Keychain, and `agyp` backs it up to `keychain_token.json` after every run.
- Per-profile locks (`~/.gemini-profiles/locks/<name>.lock`) are held only while the Keychain is prepared. If a profile is busy you will see `... is in use by another agyp command; waiting...`.

### Linux

- Off macOS `agy` stores its login either in the desktop keyring (Secret Service over D-Bus) or, when it detects an SSH session, a container or no keyring, in a token file inside its data directory. The keyring is one shared item for the whole user session, but the file lives under `--gemini_dir`, which `agyp` already sets per profile: `~/.gemini-profiles/profiles/<name>/antigravity-cli/antigravity-oauth-token`.
- `agyp` therefore always makes `agy` use the file: if neither `SSH_CONNECTION` nor `SSH_TTY` is set, it starts `agy` with `SSH_TTY=/dev/null`, which is what `agy` checks before deciding to skip the keyring. HOME is left alone, so there is no `homes/` directory and tools inside a task (git, `gh`, npm, ...) see your normal configuration and credentials.
- The token file is the source of truth for the profile. The first run seeds it from `keychain_token.json`, `agy` refreshes it in place, and `agyp` backs it up to `keychain_token.json` after every run, exactly as on macOS.
- `agyp login <name> --import-current` imports the login of plain `agy` from `~/.gemini/antigravity-cli/antigravity-oauth-token`, or from the desktop keyring when `secret-tool` is installed.
- A token saved from the macOS Keychain (go-keyring's base64 envelope) is unwrapped to plain JSON before it is written to the token file.
- `agyp default <name>` only changes the default profile on Linux; it does not replace the login of plain `agy`.

**Caveat (macOS):** tools inside a task that store credentials in the macOS Keychain (for example `gh`, or git over HTTPS with `osxkeychain`) will not see the credentials from your real login Keychain. Use SSH for git and `GH_TOKEN` for `gh`.

---

## codexp — Codex CLI with multiple ChatGPT accounts

`codexp` is installed alongside `agyp` by `make install`. Each profile gets its own `CODEX_HOME` at `~/.codex-profiles/homes/<name>/`. No Keychain is involved, so profiles run in parallel freely.

**`codexp` never logs in and never refreshes tokens.** You log the account in somewhere else (for example an account manager such as OmniRoute), export its Codex `auth.json`, and import that file:

```bash
codexp import c1 --from ~/Downloads/auth-a.json --remove-source
codexp sync ~/omniroute-exports  # re-import every *.json in a folder; new accounts become profile-<n>
codexp list
codexp q                     # Codex quota for every profile (5-hour and weekly windows)
codexp exec c1               # open the Codex TUI as c1
codexp active c2             # make plain `codex` (~/.codex, IDE extension, Codex app) use c2
codexp task --profile c1 --worktree ../wt-a --prompt-file prompt.md --timeout 30m --json
```

| Command | Description |
| :--- | :--- |
| `codexp import <name> --from <file>` | Import or refresh a profile from an exported `auth.json`. `--force`, `--remove-source` |
| `codexp sync <folder>` | Re-import every `*.json` export in a folder. An account that already has a profile is imported into it (matched by account id, then email); a new account becomes the next `profile-<n>` and is asked for an alias. The freshest export of an account wins; if plain `codex` uses a synced account its login is refreshed too |
| `codexp list` | List profiles and token validity |
| `codexp rm <name>` | Remove a profile and its `CODEX_HOME` |
| `codexp rename <old> <new>` | Rename a profile: its `CODEX_HOME` moves, aliases follow, and threads it started keep resuming. Refused while a `codexp exec`/`task` run uses it |
| `codexp alias ...` | Manage aliases |
| `codexp task --profile <p> --worktree <path>` | Run `codex exec` in a Git linked worktree. Options: `--prompt-file`, `--timeout`, `--model`, `--reasoning-effort`, `--sandbox` (default `workspace-write`), `--resume <thread_id>`, `--json` |
| `codexp q [profile] [--json]` | Read-only ChatGPT usage query (same source as Codex `/status`) |
| `codexp active [profile]` | Copy a profile's credentials into `~/.codex/auth.json` so plain `codex` uses it; without a profile, show which one it uses. A login `codexp` did not write is backed up once to `~/.codex/auth.json.pre-codexp`. Running plain `codex` CLI sessions (not ones `codexp` launched, not the Codex app or editor extensions) are stopped and a running Codex app-server daemon is restarted, because both keep the login they started with; continue them with `codex resume`. Refresh stays disabled, so re-run after re-importing, and do not run `codex login`/`logout` there |
| `codexp exec <profile> [-- <args...>]` | Run any `codex` command under a profile. The TUI, `exec`, `resume` and `fork` get `--dangerously-bypass-approvals-and-sandbox` unless you pass `-a`/`-s`/`--full-auto` yourself |

How it protects the upstream session:

- On import the refresh token is replaced with a dummy value and Codex's refresh/revoke endpoints are pointed at a dead address, so `codexp` can never rotate or revoke the token the upstream owner is still using.
- The trade-off: when the access token expires (roughly 7–10 days), the profile reports `unauthenticated` (exit code `2`). Export a fresh file and run `codexp import <name> --from <file>` again, or export every account into one folder and run `codexp sync <folder>`.
- Import rejects files whose `id_token` / `access_token` / `account_id` belong to different accounts, expired files, switching the account inside an existing profile, and importing the same account twice (unless `--force`).
- Shared Codex config lives at `~/.codex-profiles/config.toml`. `codexp` always forces `model_provider="openai"`, `cli_auth_credentials_store="file"` and `forced_login_method="chatgpt"`. `AGENTS.md` and `rules` are symlinked from `~/.codex`.
- Claude Code is the source of skills and MCP servers. On every run each profile's `skills/` gets one link per skill in Claude's `skills/` and in its enabled plugins (skills Codex already reads from `~/.agents/skills` are skipped), and Claude's MCP servers (enabled plugins' `.mcp.json`, user-scope `mcpServers` and local-scope `projects.<cwd>.mcpServers` of `.claude.json`) are passed to Codex as `-c mcp_servers.*` overrides. Secrets (server `env`, HTTP `headers`) travel through the environment, never the command line. `${VAR}` / `${VAR:-default}` are expanded; SSE servers are skipped. Claude's config is read from `$CLAUDE_CONFIG_DIR` when set, otherwise `~/.claude` and `~/.claude.json`. OAuth servers (e.g. Figma) need a one-time `codexp exec <p> -- mcp login <name>`.
- Session history is one store shared by plain `codex` and every profile: the user's own `~/.codex`. Each profile's `sessions`, `archived_sessions`, `thread-writer-locks`, `history.jsonl` and `session_index.jsonl` link into `~/.codex`, and `CODEX_SQLITE_HOME` points at `~/.codex`, so the resume picker shows the same threads for the current directory whether you run `codex`, `codexp exec c2 -- resume` or `codexp task --profile c2 --resume <thread_id>`. History from the earlier `~/.codex-profiles/shared/` layout is merged in on the next run (thread index rows are copied with `sqlite3`; the old databases stay in `~/.codex-profiles/shared.migrated/`). `codexp rm` repoints the rollout paths of threads the removed profile started.

---

## Agent skills

The repo ships two skills for coding agents:

- `skills/agyp` — plan a requirement and delegate it to AGY across several accounts.
- `skills/codexp` — delegate coding tasks to Codex workers pinned to specific accounts.
- `skills/claude-worker` — fallback: delegate to Claude Code on Sonnet 5.5 when every AGY profile is out of quota.

Install them for every agent on the machine:

```bash
make install-skills          # same as: bash scripts/install-skills.sh
```

The script symlinks each skill into:

| Directory | Agent |
| :--- | :--- |
| `~/.agents/skills` | Codex and other agents that read the shared skills folder |
| `~/.claude/skills` | Claude Code (skipped if not installed) |
| `~/.gemini/antigravity/skills` | Antigravity / Hermes (skipped if not installed) |

- Because they are symlinks, a `git pull` updates every agent. Restart the agent session to reload skills.
- An existing non-symlink skill with the same name is renamed to `<name>.bak-<timestamp>`, never deleted.
- Options: `--copy` (copy instead of symlink), `--dry-run`, `--target <dir>`, install only some skills (`scripts/install-skills.sh agyp`). Remove them with `make uninstall-skills`.

The `agyp` skill is written for the Orca worktree manager: the coordinator agent confirms a task list with you once, creates one Orca worktree per task, runs `agyp exec` in it on the profile with the most quota left, has a second AGY session review the result, and leaves the worktree for you to review. It never commits, opens PRs or deletes worktrees on its own.

### Using agyp from a custom agent (e.g. Hermes)

See [HERMES_GUIDE.md](HERMES_GUIDE.md) for a full guide. A minimal system-prompt snippet:

```markdown
You have access to `agyp` (Antigravity Multi-Profile CLI).

1. List profiles: `agyp list --json`
2. Check quota before assigning work: `agyp quota --all --json` or `agyp q`
3. For coding tasks, always use an isolated Git linked worktree:
   `agyp task --profile <profile> --worktree <path> --prompt-file <file> --timeout <duration> --json`
4. Use `agyp prompt <profile> "<text>"` only for non-coding prompts.
5. Exit code 2 means the profile is unauthenticated: ask the user to run `agyp login <profile>`.
6. On 429 / resource exhausted, keep the profile pinned, inspect the worktree, and report.
   Never switch profiles automatically.
```

---

## Development

```bash
make build      # cargo build --release
make test       # cargo test
```

## License

[MIT](LICENSE)
