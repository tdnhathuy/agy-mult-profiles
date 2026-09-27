# Hermes / AI Agent Integration Guide

> **Safety policy:** select one explicit profile for each task. Do not round-robin or automatically switch profiles after `429`/quota errors. AGY coding must run in an isolated worktree; Hermes owns review, tests, commits, merges, and pushes.

Tài liệu này được tối ưu để bạn copy trực tiếp vào System Prompt / Instructions của Hermes trên máy Mac Mini, hoặc lưu thành file hướng dẫn cho Hermes tự đọc.

---

## 1. System Prompt Snippet dành cho Hermes

```markdown
### ANTIGRAVITY MULTI-PROFILE CLI (`agyp`) CAPABILITIES

You have access to the CLI tool `agyp` installed at `~/.local/bin/agyp`.
Use `agyp` to execute AGY prompts through one explicitly selected Google account/profile at a time.

#### Core CLI Commands:
1. List available profiles and linked emails:
   agyp list --json

2. Run a delegated coding task (RECOMMENDED):
   agyp task --profile <profile> --worktree <linked-worktree-path> --prompt-file <path> [--timeout 20m] [--skip-permissions] [--json]
   Example:
   agyp task --profile p1 --worktree /path/to/worktree --prompt-file prompt.txt --json
   Returns:
   {
     "profile": "p1",
     "worktree": "/path/to/worktree",
     "canonical_worktree": "/path/to/worktree",
     "status": "success",
     "failure_kind": null,
     "agent_failure_kind": null,
     "restore_error": null,
     "agent_exit_code": 0,
     "cli_exit_code": 0,
     "exit_code": 0,
     "timed_out": false,
     "duration_ms": 12450,
     "stdout": "...",
     "stderr": ""
   }

3. Run a non-interactive prompt on a profile or alias:
   agyp prompt <profile_or_alias> "<prompt_text>"
   Example:
   agyp prompt p1 "Summarize git diff"

4. Specify model:
   agyp task --profile p1 --worktree /path/to/worktree --model gemini-2.5-flash --prompt-file prompt.txt

5. Pipe large inputs / files:
   cat prompt.txt | agyp task --profile p1 --worktree /path/to/worktree --json

#### Error Handling & Failure Kinds (`agyp task`):
- `cli_exit_code`: 0 on success, nonzero on any CLI or task failure (1 for general/setup error, 2 for unauthenticated).
- `agent_exit_code`: Direct exit code of child AGY process; null on timeout or pre-execution setup errors; preserved when post-execution Keychain restoration fails.
- `exit_code`: Deprecated backward-compatible exit code, always serialized; null on timeout or pre-execution setup errors; restoration failure classified as `setup_error` preserves child exit code.
- `restore_error`: Non-null string if Keychain restoration failed (takes precedence as `setup_error` / exit code 1).
- `agent_failure_kind`: Preserves original child failure category if Keychain restoration fails simultaneously.
- Failure categories (`failure_kind` in JSON):
  - `empty_output`: Child exited 0 but produced empty stdout.
  - `permission_denied`: Tool permissions were denied or rejected.
  - `quota_exhausted`: HTTP 429 / RESOURCE_EXHAUSTED / quota limit hit.
  - `timeout`: Overall task deadline or execution exceeded `--timeout`.
  - `agent_failed`: AGY process crashed or returned nonzero error code.
  - `invalid_worktree`: Directory is not a valid linked git worktree or forged gitdir.
  - `unauthenticated`: Profile is missing required `keychain_token.json` (exit code 2).
  - `setup_error`: Process lock acquisition, initialization, or Keychain restoration failure.
  - `invalid_duration`: Malformed `--timeout` string.
  - `invalid_prompt`: Empty prompt supplied.

#### Rate Limiting Policy:
If the selected profile encounters `RESOURCE_EXHAUSTED` / `429`, keep the same profile, inspect worktree state before any retry, and report the blocker. Never switch accounts automatically.
```

---

## 2. Các ví dụ Hermes có thể chạy trực tiếp

### Kiểm tra tài khoản nào đang sẵn sàng:
```bash
agyp list --json
```

### Prompt profile 1:
```bash
agyp prompt p1 "Tóm tắt 3 tính năng mới nhất của dự án này"
```

### Prompt profile 2 với mô hình cụ thể:
```bash
agyp prompt p2 --model gemini-2.5-pro "Review kiến trúc hệ thống"
```

### Đọc file mã nguồn và phân tích:
```bash
cat src/main.rs | agyp prompt p1 "Kiểm tra xem có bug nào tiềm ẩn không"
```
