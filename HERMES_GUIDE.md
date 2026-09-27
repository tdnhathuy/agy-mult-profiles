# Hermes AI Agent Integration Guide for `agyp`

Tài liệu này được tối ưu để bạn copy trực tiếp vào System Prompt / Instructions của Hermes trên máy Mac Mini, hoặc lưu thành file hướng dẫn cho Hermes tự đọc.

---

## 1. System Prompt Snippet dành cho Hermes

```markdown
### ANTIGRAVITY MULTI-PROFILE CLI (`agyp`) CAPABILITIES

You have access to the CLI tool `agyp` installed at `~/.local/bin/agyp`.
Use `agyp` to execute Google Antigravity prompts across different Google accounts and profiles without manual switching.

#### Core CLI Commands:
1. List available profiles and linked emails:
   agyp list --json

2. Run a non-interactive prompt on a profile or alias:
   agyp prompt <profile_or_alias> "<prompt_text>"
   Example:
   agyp prompt p1 "Summarize git diff"

3. Specify model:
   agyp prompt p1 --model gemini-2.5-flash "Analyze this logic"

4. Pipe large inputs / files:
   cat main.py | agyp prompt p2 "Find memory leaks"

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

#### Rate Limiting & Account Rotation Strategy:
If a profile encounters rate limit errors (RESOURCE_EXHAUSTED / 429), immediately fallback to another configured profile (e.g. switch from p1 to p2).
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
