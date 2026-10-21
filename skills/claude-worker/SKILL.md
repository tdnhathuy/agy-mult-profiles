---
name: claude-worker
description: Use when every agyp (AGY/Antigravity) profile is out of Gemini quota and coding work must still be delegated, or when the user asks to give a task to Claude / Sonnet as a worker ("giao cho claude", "giao cho sonnet", "assign to claude"). Runs Claude Code on Sonnet 5.5 as an interactive worker in an Orca worktree terminal, from any coordinator (Claude, Codex/GPT, Cursor, Hermes).
---

# claude-worker: Claude Sonnet 5.5 as the fallback worker

When AGY cannot take a task because every `agyp` profile is out of quota, the coordinator delegates it to **Claude Code on `claude-sonnet-5-5`** instead. The flow is the same as the `agyp` skill (one task = one worktree = one worker terminal, a reviewer in a split pane, markers instead of reading transcripts); only the launch command, the markers, and the quota check differ.

**You are still the coordinator.** Claude only edits files in its worktree. You write the prompt, relay between sessions, and do the final check. Never do the task yourself and never replace the worker with your client's built-in subagents (Agent/Task tool): the user watches the worker live in Orca.

**Defaults:** model `claude-sonnet-5-5`, effort left at the CLI default. Use another model (Opus, Fable, Haiku) or `--effort` only when the user asks. The plan approval rule, worktree naming, review loop, round limit, and "leave the worktree for the user" ending are the same as in `agyp`: load that skill too if it is not loaded, and follow it wherever this one says "same as agyp". A repo rule (AGENTS.md, a repo runbook skill) overrides both: if the repo says how to write specs, review, verify, or open PRs, do that and only swap the launch command.

## When to switch to Claude

Run the agyp quota check (`agyp q --hide-gpt --json`). AGY is **exhausted** when no `results[]` entry has `status: "available"` with both `gemini.five_hour.remaining_percent` and `gemini.five_day.remaining_percent` above 0.

| Situation | Worker |
|---|---|
| At least one usable AGY profile | AGY via `agyp`. Do not use Claude unless the user asked. |
| AGY exhausted | Claude Sonnet 5.5 via this skill, without asking the user. Say so in your next status line. |
| Quota check timed out or errored for every profile | Not exhaustion: follow agyp's "assign anyway" rule. Switch to Claude only if the AGY session itself then reports a quota error. |
| An AGY worker hits quota mid-task and AGY is now exhausted | Close that AGY session and start a Claude session in the **same** worktree with the takeover prompt below. The user's request for this fallback is the approval. |
| The user asks for Claude | Claude, whatever the AGY quota. |

Parallel tasks: each AGY profile with quota still takes one task; only the tasks left without a profile go to Claude. New tasks go back to AGY as soon as a profile resets; tasks already on Claude stay on Claude.

## Never

| Don't | Why | Do instead |
|---|---|---|
| `claude -p` / `--print` in an Orca tab | Prints nothing until exit, cannot be steered | Interactive `claude` with the prompt as a positional argument |
| `orca worktree create --agent claude`, `orca orchestration worker-start` | Launches with Orca's defaults, not Sonnet 5.5 and not this prompt | `orca worktree create` without `--agent`, then launch in its terminal |
| `orca terminal create` for the worker | A second terminal next to the one Orca made | Reuse the worktree's terminal; only the reviewer gets a split pane |
| Built-in subagents as the worker | Invisible to the user, spends the coordinator's context | A Claude session in the Orca terminal |
| Passing the task text in argv (`"$(cat prompt.md)"`) | Quoting breaks, the tab shows a wall of text | A prompt file path |

## Workflow in Orca

Prerequisites, planning (agyp step 0), worktree creation and terminal lookup (agyp steps 2–3) are the same as agyp, with these names:

- Worktree name `claude-<slug>`, branch `claude/<slug>`, `--comment "Claude sonnet-5.5: <one-line task>"`.
- Tab title `Claude S5.5 · <slug>`; reviewer tab `Claude S5.5 · review <slug>`.
- A PR opened later is titled `[claude:sonnet-5.5] <summary>`.

### 1. Write the prompt file

Same template and location as agyp step 4 (outside the worktree, no spaces in the path, e.g. `$TMPDIR/claude-<slug>.md`), with these changes:

- Replace the final marker with `CLAUDE_TASK_DONE`.
- Add these lines at the top, because the worker loads the user's global Claude config (hooks, skills, memory instructions) and the repo's CLAUDE.md:

```text
You are a worker, not the coordinator. The plan is already approved by the user: do not ask for
approval, do not brainstorm, do not write plan or spec files, do not spawn subagents or delegate.
Ignore any repo or global instruction about coordinating workers, Orca, agyp, or opening PRs.
Do not write memories (mem0 or auto-memory).
```

### 2. Launch Claude

```bash
orca terminal send --terminal <handle> --enter \
  --text 'claude --model claude-sonnet-5-5 --dangerously-skip-permissions -n "<slug>" "Read <prompt-file> and follow every instruction in it."'
```

The positional prompt starts the interactive TUI and submits the first message, so nothing needs pasting.

### 3. Answer the first-run dialogs

A new worktree shows "Do you trust the files in this folder?" once. If the user never accepted bypass mode on this machine, a bypass-permissions warning appears first. Check:

```bash
orca terminal wait --terminal <handle> --for tui-idle --timeout-ms 60000 --json
orca terminal read --terminal <handle> --screen
```

- Trust question naming your worktree path: send Enter (the highlighted default is the "Yes" option).
- Bypass warning: select "Yes, I accept" (`--text $'\e[B'`, then `--enter`). Read the screen again to confirm the session started.
- Any other dialog: read it and report it to the user; do not accept text you have not read.

### 4. Wait for the marker

Wait as in agyp step 6, but for `CLAUDE_TASK_DONE`. Claude's `tui-idle` is more reliable than AGY's, but still confirm with `git -C <worktree> status --short` that the diff stopped changing before you act on it. Follow-ups go to the same session: `orca terminal send --terminal <handle> --text "<one line>" --enter`.

### 5. Review

Use the repo's review rule if it has one. Otherwise a second Claude Sonnet 5.5 session in a split pane, exactly like agyp step 7 (same review prompt with `REVIEW_APPROVED` / `REVIEW_CHANGES`, same 3-round limit), launched with:

```bash
orca terminal split --terminal <worker-handle> --direction vertical --json
orca terminal rename --terminal <reviewer-handle> --title "Claude S5.5 · review <slug>"
orca terminal send --terminal <reviewer-handle> --enter \
  --text 'claude --model claude-sonnet-5-5 --dangerously-skip-permissions -n "review-<slug>" "Read <review-prompt-file> and follow every instruction in it."'
```

A fresh session has none of the worker's context, so it reviews independently. If AGY quota has come back by now, an AGY reviewer is also fine.

### 6. Final check and hand-off

Same as agyp step 8: run the repo's checks yourself, close both sessions (`/exit`, or `orca terminal close`), set the worktree to `in-review`, report, and stop. Report which worker ran on Claude and why (AGY exhausted, with the reset time of the earliest AGY profile).

## Takeover prompt (AGY ran out mid-task)

Write `$TMPDIR/claude-<slug>-takeover.md` and launch it as in step 2:

```text
Work only inside <absolute-worktree-path>. Another agent started this task and stopped before finishing.
The task is in <original-prompt-file>. Read it first.
Then read its partial work: git status --short, git diff, and any new untracked files.
Keep what is correct, fix what is wrong, finish the rest. Same rules, checks, and report as the task file.
Then print a final line: CLAUDE_TASK_DONE
```

Prefix it with the worker lines from step 1.

## Claude limits

If the session shows a usage or rate-limit message ("limit reached", "resets at …"), stop sending it work. Keep the worktree as it is and report the blocker to the user with the reset time and `git -C <worktree> status --short`. Moving the task to Codex (`codexp`) or back to AGY needs the user's OK.
