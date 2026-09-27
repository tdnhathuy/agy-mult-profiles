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

Use `agyp exec <selected-profile> -- <agy-args>` only for AGY flags unsupported by `prompt`. Do not use `--continue` unless the conversation belongs to this exact task.

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
