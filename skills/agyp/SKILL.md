---
name: agyp
description: Use when you need to query or run tasks across multiple Antigravity (agy) accounts or profiles. Supports listing profiles, selecting work/personal accounts, and running non-interactive prompts.
---

# Antigravity Multi-Profile CLI (`agyp`)

Use this skill when interacting with Google Antigravity across different accounts.

## 1. Inspect Available Profiles
```bash
agyp list --json
```
Returns a JSON object with all configured profiles, their linked Google emails, and aliases.

## 2. Execute a Prompt on a Specific Profile
To send a prompt non-interactively without user interruption:
```bash
agyp prompt <profile_or_alias> "<prompt text>"
```

### Examples:
- Use alias: `agyp prompt p1 "Check current git status"`
- Use model flag: `agyp prompt work --model gemini-2.5-flash "Summarize changes"`
- Pipe content: `cat error.log | agyp prompt personal "Analyze this crash stacktrace"`

## 3. Advanced Pass-Through Commands
To run arbitrary `agy` options under a profile:
```bash
agyp exec <profile_or_alias> -- --continue
```

## 4. Handling Unauthenticated Errors (Exit Code 2)
If a prompt fails with exit code 2:
- The profile is not logged in.
- Prompt the user to run: `agyp login <profile>` in an interactive terminal.
