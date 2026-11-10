---
name: agy-web
description: Use when a task needs operating a website — reading or filling pages in Google Play Console, Google Cloud Console, GitHub web, Reddit, Facebook, forms or dashboards — so an AGY (Gemini) worker drives Orca's built-in browser through the orca CLI and the coordinator does not spend tokens on page reads and screenshots. Also when the user says "giao web cho agy", "agy-web", or asks AGY to check, click or fill something on a site.
---

# agy-web: give browser work to an AGY worker

A Gemini worker (`agyp prompt`, headless) drives Orca's built-in browser with the `orca` CLI: open tabs, read pages as text, fill forms, click. You (the coordinator) write a short spec, run it, read a ~10-line report, verify cheaply, and do the irreversible click yourself. The browser keeps the user's sign-ins, so the worker needs no login.

## Who does what

| Step | Who |
|---|---|
| Navigate, read status, fill forms, select options, draft text into fields | **Worker** |
| Buttons that cannot be undone or reach other people: Submit for review, Send, Publish/Post, Invite, Delete/Remove, Pay, rollout | **You**, after checking the filled page; ask the user first when it is outward-facing and they have not already asked for that exact action |
| Sign-in, 2FA, CAPTCHA, downloading secrets (keys, tokens) | **User** (and you move/secure the file) |

Do it yourself instead when it is one or two `orca` commands you already know (cheaper than a worker run, which takes 30–90 s).

## 1. Write the spec

Write it to your scratchpad (or `$TMPDIR`), path without spaces, e.g. `<scratch>/web-<slug>.md`. Fill every `<...>`:

```text
You are a browser operator. Drive Orca's built-in browser ONLY with the `orca` shell CLI
(no other browser tool, no curl to the site). Run every command from the current directory.

Goal: <one sentence>.
Start URL: <url>   (or: use the existing tab with browserPageId <id>)
Facts / values: <exact text to type, options to pick, items to read>.

Allowed: navigate, read, scroll, type into fields, tick boxes, pick options, open dialogs,
press "Next" / "Not now" / "Cancel" / "Discard"<, plus: ...>.
STOP before (do NOT click): <Submit, Send, Save, Publish, Post, Invite, Delete, Remove, Pay, ...>.
  When you reach that button, leave the tab open and report its ref and label.
If asked to sign in, solve a CAPTCHA, or confirm with 2FA: stop and report it.
Never type passwords, never download or open credential files.

Orca CLI (P = browserPageId):
- orca tab create --url <url> --json            -> result.browserPageId
- orca viewport --page P --width 1400 --height 900 --json   (do this first; narrow pages hide sidebars)
- orca goto --page P --url <url> --json ; orca wait --page P --load load --json (then sleep 3-8 s for SPAs)
- orca exec --page P --command "read" --json     page as text (prefer this to screenshots)
- orca snapshot --page P --interactive            controls with refs @eN (refs die on navigation)
- orca click|check|focus --page P --element @eN --json
- orca fill --page P --element @eN --value "<text>" --json      (whole value, any length)
- orca type --page P --input "<text>" --json      (real keys into the focused field; chip/tag inputs, then keypress Enter)
- orca keypress --page P --key Enter --json
- orca exec --page P --command "get value @eN" --json            (check what a field holds)
- orca screenshot --page P   (PNG path; only when refs fail or a layout question needs eyes)
- Mouse commands take no --page: they act on the current tab, so first `orca tab switch --page P --json`.
  orca mouse move --x X --y Y --json ; orca mouse down --json ; orca mouse up --json
  (click by screenshot coordinates when a ref says "covered by" or a custom dropdown ignores clicks)
- orca mouse wheel --dy 400 --json      (scroll a dialog or inner panel under the last mouse position)
- orca tab list --json / orca tab close --page P --json
Custom dropdowns: click the field, snapshot, click the option. After each action re-snapshot
before using refs again.

Report in exactly this format, plain text, nothing before it:
RESULT: <what you did and the facts read, short>
PAGE_ID: <browserPageId left open, or closed>
FINAL_URL: <url>
STOPPED_AT: <ref + label of the button you stopped before, or none>
BLOCKERS: <none, or what stopped you>
WEB_TASK_DONE
```

Rules for a good spec:
- Give exact values (text, which boxes, which option), never "fill it sensibly". Long text: put it in a file and say "type the content of <file>" (the worker can `cat` it).
- Name the STOP buttons for this site. Close the tab at the end only for read-only tasks.
- One site and one goal per spec. Split long flows (e.g. create group → configure track) into sequential specs.

## 2. Run it

```bash
<skill-dir>/scripts/agy-web.sh <spec-file> [profile]
```

- Run from the checkout whose browser tabs the task uses (orca targets the worktree of the cwd).
- It picks the available profile with the most Gemini quota (`AGY_WEB_PROFILE` to pin one), runs `agyp prompt --skip-permissions --json`, prints the report, saves it as `<spec>.out.txt`, and exits non-zero on failure or a missing `WEB_TASK_DONE`.
- Over ~2 minutes expected: use Bash `run_in_background` and wait for the notification.
- Parallel tasks: different sites only, one profile each. Two specs on the same site/tab must run one after the other.
- Every AGY profile out of quota: run the same spec with the Agent tool on `model: "haiku"` (subagent, prompt = "Read <spec> and follow every instruction in it."). Same report format.

## 3. Verify, then finish

The report is a claim. Check it with text, not images:

```bash
orca exec --page <PAGE_ID> --command "read" --json        # facts / status text
orca snapshot --page <PAGE_ID> --interactive              # field values, checked boxes, STOPPED_AT ref
```

- Values wrong or missing: send a corrective spec (same page id), or fix the one field yourself.
- `BLOCKERS` sign-in / CAPTCHA: tell the user exactly what to click in the Orca browser tab, wait for "xong", re-run.
- `STOPPED_AT` a final button: re-snapshot, confirm the label and the page state, then click it yourself (with user OK when outward-facing). Read the page again to confirm the result before reporting.
- Report to the user what changed, what you verified, and what still waits on them.

## Site notes

- **Google Play Console**: after most saves a "Go to Publishing overview?" dialog appears; "Not now" closes it. Saved changes only go live after "Send changes for review" on Publishing overview (an irreversible step: yours). Some checkboxes report "covered by" — click by coordinates. Country lists: search box + row checkbox, then Save.
- **Google Cloud Console**: pages need `?project=<id>`; creating a service-account key downloads a JSON secret into ~/Downloads — the coordinator moves it out of Downloads and chmods it 600, never the worker.
- **Google Groups**: "Create group" asks for a CAPTCHA at the end — user step.
- **Social posting (Reddit, Facebook)**: worker opens the composer, fills title/body/flair and attaches images by path (`orca upload --page P --element @eN --files <path> --json`), then stops at Post.

## Never

- Put passwords, tokens or keys in a spec or ask the worker to read credential files (`~/.gemini-profiles/`, `*.json` keys, `.env*`).
- Let the worker press a STOP button "because it was the only way forward" — it reports instead.
- Trust a report without one text check of the page.
