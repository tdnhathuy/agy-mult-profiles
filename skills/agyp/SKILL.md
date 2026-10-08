---
name: agyp
description: Use when delegating, assigning, or handing off coding work to AGY, Antigravity, or Gemini ("giao cho AGY", "giao cho đệ", "assign to agy"), from Orca or any coordinator agent (Claude, Codex/GPT, Hermes), or when spreading work across multiple Antigravity accounts to use more quota.
---

# agyp: delegate coding to AGY across many accounts

`agyp` runs the Antigravity CLI (`agy`) under one of several Google accounts ("profiles"). Each profile has its own isolated HOME and Keychain, so many AGY sessions run in parallel on different accounts. More profiles = more Gemini quota.

**You are the coordinator.** AGY only edits files in its worktree. You pick the profile, write the prompt, watch progress, review the diff, run the tests, and do every commit/merge/push.

**One task = one worktree = one terminal running `agyp exec <profile>`.** In Orca, the user watches every AGY session live in its worktree's own terminal.

## Never use these for AGY work

| Don't | Why | Do instead |
|---|---|---|
| `orca worktree create --agent antigravity`, `orca orchestration worker-start --agent antigravity` | Launches plain `agy` on one account; ignores all other profiles | `orca worktree create` **without** `--agent`, then `agyp exec <profile>` in its terminal |
| `orca terminal create …` for AGY | Adds a second terminal next to the one Orca already made | Reuse the worktree's existing terminal |
| raw `agy` | Uses the single system login | `agyp exec <profile> -- …` |
| `--dangerously-skip-permissions` | Not needed: trusting the worktree already allows read/edit/run inside it | Answer the trust prompt (step 5) |

## Workflow in Orca

Use this when `orca status --json` reports the runtime as `ready` and the repo shows up in `orca repo list --json`. Otherwise use "Without Orca" below.

### 1. Clarify, then pick profiles

If a task is ambiguous (missing file, unclear target), ask the user before creating anything.

```bash
agyp usage --json    # quota per profile, ~6s per profile
```

Each `results[]` entry has `profile`, `email`, `status`, and `gemini.five_hour.remaining_percent` / `gemini.five_day.remaining_percent`.

- Rank `available` profiles by the **lower** of their two Gemini percentages and pick the highest.
- Skip any profile where either window is 0.
- Give each parallel task a **different** profile.

### 2. Create the worktree

```bash
orca repo list --json      # repo id (path: selectors fail on symlinked repo paths)
orca worktree create --repo id:<repo-id> --name agy-<slug> \
  --comment "AGY <profile>: <one-line task summary>" --json
```

- The checkout path is in `result.worktree.path`.
- Add `--no-parent` when your own checkout belongs to a different repo.
- The `--comment` shows in Orca's sidebar, so the user sees which account runs which task.

### 3. Find and label its terminal

```bash
orca terminal list --worktree path:<worktree-path> --json   # handle = result.terminals[0].handle
orca terminal rename --terminal <handle> --title "AGY <profile> · <slug>"
orca worktree set --worktree path:<worktree-path> --workspace-status in-progress
```

### 4. Write the prompt file, then launch AGY with it

Write the prompt to a temp path **outside** the worktree with no spaces in the path, for example `$TMPDIR/agy-<slug>.md`. AGY can read it there; a file inside the worktree would end up in the diff. It must contain:

```text
Work only inside <absolute-worktree-path>.
Task: <what to build or fix, with file paths and acceptance criteria>.
Run the relevant tests: <exact command>.
Do not commit, merge, rebase, push, or switch branches.
Finish by reporting: files changed, tests run with results, open risks.
Then print a final line: AGY_TASK_DONE
```

Launch AGY in the worktree's terminal. `-i` starts the interactive TUI and submits the first prompt, so you don't paste anything:

```bash
orca terminal send --terminal <handle> --enter \
  --text 'agyp exec <profile> -- -i "Read <prompt-file> and follow every instruction in it."'
```

Use `--model <id>` after `--` to pick a model; `agy models` lists them.

### 5. Answer the one-time trust prompt

AGY asks "Do you trust the contents of this project?" once per new worktree. The first `wait` returns `blockedReason: "agent-trust-workspace"`:

```bash
orca terminal wait --terminal <handle> --for tui-idle --timeout-ms 60000 --json
orca terminal read --terminal <handle> --screen
```

Send Enter (the default is "Yes, I trust this folder") **only while the screen actually shows that question** and names your worktree path:

```bash
orca terminal send --terminal <handle> --enter
```

Orca can keep reporting `agent-trust-workspace` after the question is answered. Always decide from the screen, never from `blockedReason` alone. A stray Enter could approve a different question.

### 6. Wait for the result

Repeat until the screen has a line that is exactly `AGY_TASK_DONE`:

```bash
orca terminal wait --terminal <handle> --for tui-idle --timeout-ms 600000 --json
orca terminal read --terminal <handle> --screen
```

- While AGY works, `wait` may return at once with `satisfied: false`. Pause 15–30 seconds before the next round.
- `satisfied: true` alone does not mean the task is done. Only the `AGY_TASK_DONE` line does.

| What you see on screen | Action |
|---|---|
| `AGY_TASK_DONE` plus AGY's report | Go to step 7 |
| A question or approval prompt | Answer only if it concerns this worktree and the task; otherwise ask the user |
| `429`, `RESOURCE_EXHAUSTED`, or a quota message | See "Quota exhausted" |
| Login or auth error | Ask the user to run `agyp login <profile>` in their own terminal |

To send a follow-up instruction to the same session: `orca terminal send --terminal <handle> --text "<one line>" --enter`.

### 7. Review and integrate (coordinator only)

1. `git -C <worktree> status --short` and `git -C <worktree> diff`. Reject unrelated files.
2. Run the repo's tests, lint, and build yourself. AGY saying "tests pass" is a claim, not proof.
3. Commit on the worktree branch, then merge or open a PR the way the repo expects.
4. `orca worktree set --worktree path:<worktree-path> --workspace-status in-review` while waiting for merge.
5. After merging: `orca worktree rm --worktree path:<worktree-path> --force --json`. This also closes the AGY terminal. Removal finishes a few seconds later.

**Status at a glance:** `orca worktree list --json` lists every `agy-*` worktree with its comment (profile and task) and status. `orca terminal read --terminal <handle> --screen` shows any session's progress.

## Quota exhausted

Keep the task on its profile and inspect `git -C <worktree> status`. Report the blocker to the user. Moving the same task to another profile requires the user's explicit OK, because the first run's partial edits are still in the worktree. Choosing a different profile for a *new* task is always fine.

## Without Orca: headless

Create a linked worktree, then run AGY non-interactively and read the JSON result:

```bash
git -C <repo> worktree add -b agy/<slug> <repo>-wt/agy-<slug>
agyp task --profile <profile> --worktree <worktree-path> --prompt-file <prompt-file> --timeout 30m --json
```

- Use the same prompt contract as step 4. You can drop the `AGY_TASK_DONE` line.
- Headless mode denies every shell command unless `--skip-permissions` is passed. Pass it only if the user allows it. Otherwise tell AGY not to run commands, and run the tests yourself.
- Result fields:
  - `status: "success"` with exit code `0` means AGY finished; `stdout` holds its report.
  - Other outcomes set `failure_kind`: `unauthenticated` (exit 2), `quota_exhausted`, `empty_output`, or `timeout`.
  - With `empty_output`, if `stderr` mentions the `"command" permission`, AGY tried to run a shell command and was blocked.
- Run parallel tasks as background jobs with one output file each. Clean up with `git worktree remove <path>` and `git branch -d <branch>`.

## Other commands

```bash
agyp list --json                    # profiles and emails
agyp q [<profile>]                  # human-readable quota with reset times
agyp alias set p1 profile-1         # short names; work anywhere a profile is accepted
agyp login <profile>                # user only: interactive Google OAuth in a browser
```

Never read, print, or copy files under `~/.gemini-profiles/` (they hold credentials).
