---
name: agyp
version: 0.2.0
description: "Use when delegating coding to AGY through one explicit Antigravity profile. Pins the profile, serializes credentials, isolates edits in a worktree, and leaves review/integration to Hermes."
platforms: [macos]
metadata:
  hermes:
    tags: [coding-agent, antigravity, multi-profile, delegation, worktree]
    related_skills: [agentic-development-orchestration, isolated-worktree-development, requesting-code-review]
---

# AGY Multi-Profile Delegation

Use `agyp` to delegate implementation to AGY while Hermes/SOL owns planning, review, testing, commits, merges, and pushes.

## Preconditions

1. Verify binaries and profiles:
   ```bash
   command -v agyp && agyp --version
   command -v agy && agy --version
   agyp list --json
   ```
2. Select one explicit authenticated profile from the list and pin it for the task.
3. Never omit the profile, use implicit defaults, round-robin, or automatically switch after `429`.
4. For coding, create or select an isolated Git worktree. Verify its branch and clean/known status before execution.

`agyp` serializes profile/Keychain activation across processes. Multiple callers may queue, but AGY profile sessions run one at a time under the shared macOS Keychain.

## Invocation

### Recommended: `agyp task` (Production-Hardened Task Delegation)

For agentic delegation, **always prefer `agyp task`**. It strictly validates that `--worktree` is an isolated linked Git worktree, sets AGY's working directory, drains stdout/stderr concurrently to avoid OS pipe deadlocks, enforces timeouts, and classifies outcomes into machine-readable JSON:

```bash
agyp task --profile <selected-profile> --worktree <absolute-worktree-path> --prompt-file <prompt-path> [--timeout 20m] [--model <model>] [--skip-permissions] [--json]
```

Or pass prompts via stdin:

```bash
printf '%s\n' '<scoped task>' | agyp task --profile <selected-profile> --worktree <absolute-worktree-path> --json
```

Key features of `agyp task`:
- **Explicit Profile**: `--profile` is mandatory; never falls back to defaults or rotates accounts.
- **Linked Worktree Validation**: Rejects canonical/main checkouts, non-worktree dirs, subdirectories, and symlinks.
- **Concurrent Stream Draining**: Reader threads drain stdout and stderr immediately from process start so large outputs cannot deadlock the OS pipe.
- **Success Gate**: Requires child exit 0, trimmed stdout non-empty, and absence of permission denial or quota exhaustion markers.
- **Classified Failure Outcomes**: JSON includes `status`, `failure_kind`, `agent_failure_kind` (when restoration fails after child error), `restore_error` (Keychain restoration failure error), `agent_exit_code` (child process status), `cli_exit_code` (nonzero on all failures), backward-compatible `exit_code`, `timed_out`, `duration_ms`, `stdout`, and `stderr`.
- **Keychain Safety**: Restores the original Keychain state on all paths (success, error, timeout, interrupt). Restoration failure takes precedence and fails the command with exit code 1 (`setup_error`).

### Legacy / Quick Prompt: `agyp prompt`

Run AGY with the isolated worktree as the terminal tool's `workdir`:

```bash
agyp prompt <selected-profile> "<scoped task>"
```

Permission auto-approval is **off by default**. Only with explicit user authorization, and only inside a disposable isolated worktree, add:

```bash
agyp prompt <selected-profile> --skip-permissions "<scoped task>"
```

`--add-dir` adds context; it does not change AGY's working directory.

For stdin input, omit the positional prompt:

```bash
{ printf '%s\n\n' '<instruction>'; cat <file>; } | agyp prompt <selected-profile>
```

Use `agyp exec <selected-profile> -- <agy-args>` only for AGY flags unsupported by `prompt` or `task`. Do not use `--continue` unless the conversation belongs to this exact task.

## Delegation Contract

Every coding prompt must state:

- Work only inside `<absolute-worktree-path>`.
- Inspect, implement, and run relevant tests.
- Do not touch the canonical checkout or `main`/`master`.
- Do not commit, merge, rebase, cherry-pick, push, or manage remotes.
- Report changed files, exact tests run/results, and unresolved risks.

Hermes/SOL independently inspects the complete diff and reruns repository gates. A successful AGY exit is not an integration approval.

## Rate Limits and Failures

- On authentication failure, stop and ask the user to run `agyp login <profile>` interactively. Never automate Google login or handle credentials directly.
- On `429` or resource exhaustion, do not switch profiles and do not blindly rerun a mutating prompt. Inspect worktree state first; stop and report the blocker unless the user explicitly authorizes a bounded retry on the same profile.
- Never print, read back, or include token files in prompts, logs, diffs, or chat.

## Parent Review Gate

After AGY returns:

1. Inspect `git status --short --branch` and the complete diff.
2. Reject unrelated files or ownership violations.
3. Run the repository's format, lint, typecheck, test, and build gates as applicable.
4. Independently review security-sensitive changes.
5. Only Hermes commits, integrates, merges, or pushes.
