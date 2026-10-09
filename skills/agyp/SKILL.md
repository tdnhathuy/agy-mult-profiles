---
name: agyp
description: Use when the user gives a requirement to be planned and delegated to AGY, Antigravity, or Gemini ("giao cho AGY", "giao cho đệ", "assign to agy"), or delegates, assigns, or hands off coding work to them, from Orca or any coordinator agent (Cursor, Claude, Codex/GPT, Hermes), or when spreading work across multiple Antigravity accounts to use more quota.
---

# agyp: delegate coding to AGY across many accounts

`agyp` runs the Antigravity CLI (`agy`) under one of several Google accounts ("profiles"). Each profile has its own isolated HOME and Keychain, so many AGY sessions run in parallel on different accounts. More profiles = more Gemini quota.

**You are the coordinator.** AGY only edits files in its worktree. You pick the profiles, write the prompt, relay between sessions, and do the final check. Code review is done by a second AGY session, not by you: this saves your tokens.

**One task = one worktree = a worker session + a reviewer session.** The worker runs in the worktree's own terminal; the reviewer runs in a split pane next to it. In Orca, the user watches both live.

**The coordinator can be any model** (Cursor, claude-omni, Claude, Codex, Hermes…): the steps below are the same whichever one you are.

**Defaults:** the AGY model is `gemini-3.8-flash-high` (fast and reliable for scoped tasks). When the reviewer approves, you close both sessions and **leave the worktree for the user to review**. Never open a PR, merge, or delete the worktree on your own; the user decides (step 8).

## Never use these for AGY work

| Don't | Why | Do instead |
|---|---|---|
| `orca worktree create --agent antigravity`, `orca orchestration worker-start --agent antigravity` | Launches plain `agy` on one account; ignores all other profiles | `orca worktree create` **without** `--agent`, then `agyp exec <profile>` in its terminal |
| `orca terminal create …` for the worker | Adds a second terminal next to the one Orca already made | Reuse the worktree's existing terminal; only the reviewer gets a split pane (step 7) |
| raw `agy` | Uses the single system login | `agyp exec <profile> -- …` |

## Workflow in Orca

Use this when `orca status --json` reports the runtime as `ready` and the repo shows up in `orca repo list --json`. Otherwise stop and tell the user Orca isn't ready; there is no other way to run AGY tasks.

### 0. Plan with the user (the only approval gate)

The user usually gives a rough requirement. Turn it into a task list they approve **once**; after that you run everything without asking again.

1. **Load the skills the requirement needs.** Check which skills you have and load the relevant ones: e.g. performance, debugging, frontend/design, impact analysis, the repo's own skills. For a large or vague requirement, load `superpowers:brainstorming` if available, but use only its questioning; do not write a spec file or commit. A skill you don't have is simply skipped.
   - If you load `superpowers:writing-plans`, skip its execution-choice step: execution is always AGY via this skill, never Claude subagents or inline.
2. **Scout the code** yourself: files to change, the existing pattern to copy, the test command. AGY should never have to search for what you already know. This is what avoids re-runs.
3. **Present one short note** and wait:
   - Your understanding of the requirement (what the user said vs. what you assume).
   - Open questions, if any.
   - The task list, one line each: goal · files · test command · depends on (task # or none).

   Split so each task fits one AGY pass and tasks touch different files. If two tasks must edit the same file, make one depend on the other.
4. **Do not create any worktree until the user approves.** If they change something, update the note and ask again.

After approval: write every task's prompt (step 4), then run steps 1–8 for each task with no further questions to the user. Start all tasks with no unmet dependency in parallel; start a dependent task only after its dependency reaches step 8. From here you only wait on markers and relay one-liners. Ask the user again only for a blocker (auth, quota, round limit, a prompt outside the task's scope).

There is no plan file in the repo: the prompt files in `$TMPDIR` are the plan, and the approved note lives in the chat.

### 1. Pick profiles

```bash
agyp usage --json    # quota per profile, ~6s per profile
```

Each `results[]` entry has `profile`, `email`, `status`, and `gemini.five_hour.remaining_percent` / `gemini.five_day.remaining_percent`.

- Rank `available` profiles by the **lower** of their two Gemini percentages and pick the highest.
- Skip any profile where either window is 0.
- Give each parallel task a **different** worker profile.
- Pick the reviewer profile at step 7, when the worker finishes (quota and free profiles may have changed by then).

### 2. Create the worktree

```bash
orca repo list --json      # repo id (path: selectors fail on symlinked repo paths)
orca worktree list --json  # stale agy-* worktrees? ask the user before adding more; keep ≤ 5 open
orca worktree create --repo id:<repo-id> --name agy-<slug> \
  --comment "AGY <profile>: <one-line task summary>" --json
```

- The checkout path is in `result.worktree.path`.
- Branch name: `agy/<slug>` (rename with `git -C <worktree> branch -m agy/<slug>` if Orca chose another). If the user later opens a PR, title it `[agy:<profile>] <summary>` so the source is traceable.
- Add `--no-parent` when your own checkout belongs to a different repo.
- The `--comment` shows in Orca's sidebar, so the user sees which account runs which task.

### 3. Find and label its terminal

```bash
orca terminal list --worktree path:<worktree-path> --json   # handle = result.terminals[0].handle
orca terminal rename --terminal <handle> --title "AGY <profile> · <slug>"
orca worktree set --worktree path:<worktree-path> --workspace-status in-progress
```

### 4. Write the prompt file, then launch AGY with it

Use what you found while scouting in step 0. Write the prompt to a temp path **outside** the worktree with no spaces in the path, for example `$TMPDIR/agy-<slug>.md`. AGY can read it there; a file inside the worktree would end up in the diff. Template:

```text
Work only inside <absolute-worktree-path>.

Goal: <one sentence>.
Context: <2-4 lines: why, what exists today>.

Files to change:
- <path>: <what to do there>
Files to read for the pattern (copy its style): <path>
Do NOT touch: <paths or areas>.

Acceptance criteria:
- [ ] <observable, checkable result>
- [ ] <…>

Verify by running: <exact test/lint/build command>. Fix failures before reporting.

Constraints: no refactors outside the listed files, no new dependencies, keep existing style.
Do not commit, merge, rebase, push, or switch branches.

Finish by reporting: files changed, command output summary, open risks or assumptions.
Then print a final line: AGY_TASK_DONE
```

Before sending, check: every path exists, acceptance criteria are checkable, and the prompt matches the approved task line. If the task turns out bigger than one pass, stop and bring the split back to the user (step 0) instead of launching it.

Launch AGY in the worktree's terminal. `-i` starts the interactive TUI and submits the first prompt, so you don't paste anything:

```bash
orca terminal send --terminal <handle> --enter \
  --text 'agyp exec <profile> -- --dangerously-skip-permissions --model gemini-3.8-flash-high -i "Read <prompt-file> and follow every instruction in it."'
```

Change the model only if the user asks; `agy models` lists the options.

### 5. Answer the one-time trust prompt

Workers and reviewers run with `--dangerously-skip-permissions` (yolo), so tool approvals never block them. The trust prompt below may still appear; handle it only if the screen shows it.

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
| `AGY_TASK_DONE` plus AGY's report | Go to step 7 (don't read the diff yourself) |
| A question or approval prompt | Answer only if it concerns this worktree and the task; otherwise ask the user |
| `429`, `RESOURCE_EXHAUSTED`, or a quota message | See "Quota exhausted" |
| Login or auth error | Ask the user to run `agyp login <profile>` in their own terminal |

To send a follow-up instruction to the same session: `orca terminal send --terminal <handle> --text "<one line>" --enter`.

### 7. Cross-review by a second AGY session

**Pick the reviewer profile:** run `agyp usage --json` again and use the same ranking as step 1, but prefer a profile that is **not the worker's** and not running another task. If every other profile is busy or out of quota, use the worker's own profile in a **new** session; a fresh session has none of the worker's context, so it still reviews independently.

**Write the review prompt** to `$TMPDIR/agy-<slug>-review-prompt.md`:

```text
You are reviewing, not coding. Work only inside <absolute-worktree-path>. Do NOT edit any file in it.

The task given to the developer is in <prompt-file>. Read it first.
Review the uncommitted changes: git status --short, git diff (and new untracked files).
Run: <exact test/lint/build command>.

Check: every acceptance criterion is met; nothing outside "Files to change" was touched;
no bugs, missing edge cases, or style that breaks the existing pattern; the command passes.

Write your findings to <review-file> (overwrite it), one item per line:
  <path>:<line> - <problem> - <what to change>
Only real problems that must be fixed; no praise, no optional nits.

Then print exactly one final line:
  REVIEW_APPROVED   (no items)
  REVIEW_CHANGES    (items written to the review file)
```

`<review-file>` is `$TMPDIR/agy-<slug>-review.md`, outside the worktree.

**Launch the reviewer** in a split pane of the worker's terminal:

```bash
orca terminal split --terminal <worker-handle> --direction vertical --json   # reviewer handle in the result (or: orca terminal list)
orca terminal rename --terminal <reviewer-handle> --title "AGY <reviewer-profile> · review <slug>"
orca terminal send --terminal <reviewer-handle> --enter \
  --text 'agyp exec <reviewer-profile> -- --dangerously-skip-permissions --model gemini-3.8-flash-high -i "Read <review-prompt-file> and follow every instruction in it."'
```

A different profile asks the trust question again: answer it as in step 5. Then wait as in step 6, but for `REVIEW_APPROVED` or `REVIEW_CHANGES`.

**The loop.** Read only the marker lines, never the diff or the review file contents:

| Reviewer prints | You do |
|---|---|
| `REVIEW_APPROVED` | Go to step 8 |
| `REVIEW_CHANGES` | Send the worker one line: `Read <review-file> and fix every item in it. Then print AGY_TASK_DONE.` Wait for `AGY_TASK_DONE`, then send the reviewer: `The developer has made fixes. Review again from scratch, following the same instructions.` Wait for its marker. |

**Round limit:** after 3 `REVIEW_CHANGES` rounds, stop. Read the review file once, then fix it yourself or report to the user. Do not burn more quota.

### 8. Final check, close the sessions, hand the worktree to the user

The reviewer's approval is a claim, not proof. Check cheaply:

1. `git -C <worktree> status --short` and `git -C <worktree> diff --stat`. Read the full diff only for files outside the task's "Files to change".
2. Run the repo's tests, lint, and build yourself.
3. If either fails, treat it as `REVIEW_CHANGES`: write the failure to the review file and send it to the worker (counts toward the round limit).
4. Close both sessions so they stop using quota: `orca terminal close --terminal <reviewer-handle>`, then the same for `<worker-handle>`.
5. `orca worktree set --worktree path:<worktree-path> --workspace-status in-review`.
6. Report to the user: worktree path/name, worker and reviewer profiles, review rounds, files changed, test results you ran, open risks. Then **stop**.

**Do not** commit, push, open a PR, merge, or remove the worktree unless the user asks. They review the worktree in Orca and decide. Only when they say so: commit/PR/merge as the repo expects, then `orca worktree rm --worktree path:<worktree-path> --force --json` and `git branch -d <branch>` if merged.

**Status at a glance:** `orca worktree list --json` lists every `agy-*` worktree with its comment (profile and task) and status. `orca terminal read --terminal <handle> --screen` shows any session's progress.

## Quota exhausted

Keep the task on its profile and inspect `git -C <worktree> status`. Report the blocker to the user. Moving the same task to another profile requires the user's explicit OK, because the first run's partial edits are still in the worktree. Choosing a different profile for a *new* task is always fine.

## Other commands

```bash
agyp list --json                    # profiles and emails
agyp q [<profile>]                  # human-readable quota with reset times
agyp alias set p1 profile-1         # short names; work anywhere a profile is accepted
agyp login <profile>                # user only: interactive Google OAuth in a browser
```

Never read, print, or copy files under `~/.gemini-profiles/` (they hold credentials).
