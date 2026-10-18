---
name: codexp
description: Delegate coding tasks to Codex CLI subagents pinned to specific ChatGPT accounts via codexp (one isolated CODEX_HOME per imported account).
---

# codexp — Codex multi-account subagents

`codexp` runs `codex exec` under one of several imported ChatGPT accounts. Each profile has its
own `CODEX_HOME`, so profiles run fully in parallel, including several tasks on the same profile.

## Rules

1. Discover profiles and token validity:
   `codexp list --json`
   Use only profiles with `"status": "ready"` (or `"expiring"` for short tasks).
2. Check quota before assigning work (read-only, never refreshes tokens):
   `codexp q --json` (all profiles) or `codexp q <p> --json`.
   Each result has `status` (`ok` / `limit_reached` / `error`), `plan`, and `primary` /
   `secondary` windows with `remaining_percent` and `reset_at` (GMT+7). Plus accounts have a
   5-hour and a 7-day window; free accounts have a single 30-day window. Prefer profiles with
   remaining quota in both windows.
3. Delegate coding work ONLY through `codexp task` in a Git linked worktree:
   `codexp task --profile <p> --worktree <path> --prompt-file <file> --timeout 30m --json`
   (or pipe the prompt on stdin). Options: `--model`, `--reasoning-effort`,
   `--sandbox read-only|workspace-write|danger-full-access` (default `workspace-write`).
4. Read the result from the JSON: `status`, `failure_kind`, `final_message`, `thread_id`, `usage`.
   To follow up in the same conversation:
   `codexp task --profile <same p> --worktree <same path> --resume <thread_id> --prompt-file <file> --json`
   History is shared across profiles, so `--resume <thread_id>` also works under another profile
   when the user asks for that.
5. Exit codes: `0` success, `1` failure (see `failure_kind`), `2` unauthenticated, `127` codex missing.
6. `failure_kind = "unauthenticated"` (exit 2): the imported token expired. codexp never logs in or
   refreshes tokens. Tell the user to export a fresh auth file from OmniRoute and run
   `codexp import <profile> --from <file> --remove-source`. Do not try `codex login`.
7. `failure_kind = "quota_exhausted"`: keep the profile pinned, inspect the worktree state, and
   report the blocker. Never switch profiles automatically.
