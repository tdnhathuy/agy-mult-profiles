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

2. Run a non-interactive prompt on a profile or alias:
   agyp prompt <profile_or_alias> "<prompt_text>"
   Example:
   agyp prompt p1 "Summarize git diff"

3. Specify model:
   agyp prompt p1 --model gemini-2.5-flash "Analyze this logic"

4. Pipe large inputs / files (omit the positional prompt so stdin is read):
   { printf '%s\n\n' 'Find memory leaks:'; cat main.py; } | agyp prompt p2

5. Structured JSON Output:
   agyp prompt p1 --json "Run tests"
   Returns:
   {
     "profile": "p1",
     "email": "user@gmail.com",
     "status": "success",
     "exit_code": 0,
     "output": "...",
     "error": null
   }

#### Error Handling:
- Exit Code 0: Success.
- Exit Code 1: Profile or alias not found / syntax error.
- Exit Code 2: Profile is unauthenticated. Notify the user to run `agyp login <profile>` in an interactive terminal.
- Exit Code 127: `agy` binary not found in PATH.

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
