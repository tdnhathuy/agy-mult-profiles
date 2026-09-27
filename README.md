# agyp - Antigravity Multi-Profile CLI

CLI tool written in Rust to manage multiple Google Antigravity (`agy`) accounts and profiles with full credential isolation on macOS. Specifically designed for **Developers** and **AI Agents (such as Hermes)** to execute isolated prompts across multiple accounts non-interactively.

---

## ⚡ Highlights

- **True macOS Keychain Isolation**: Solves `agy`'s global Keychain auto-login collision by isolating credentials per profile (`~/.gemini-profiles/profiles/<name>/keychain_token.json`).
- **Seamless Browser OAuth**: Automatically clears Keychain prior to login so Google displays the account selection screen (`prompt=consent`).
- **AI Agent-First Ergonomics**:
  - Non-interactive prompts keep AGY permission checks enabled by default; `--skip-permissions` is explicit.
  - Zero wrapper noise: clean stdout streaming.
  - `--json` output support for programmatic agent parsing.
  - Exit code contracts (`0` = success, `1` = not found/invalid, `2` = unauthenticated, `127` = binary missing).
- **Aliases**: Map short handles (`p1`, `work`, `personal`) to profiles.

---

## 🛠️ Mac Mini Setup Guide (Dành cho User)

> **Lưu ý:** Bạn chỉ cần thực hiện các bước cài đặt và login dưới đây 1 lần trên Mac Mini. Sau khi hoàn tất, Hermes có thể tự động gọi `agyp` hoàn toàn độc lập.

### 1. Cài đặt Prerequisites
Đảm bảo Mac Mini đã cài Rust/Cargo và Antigravity CLI (`agy`):
```bash
# Kiểm tra agy
which agy
agy --version

# Kiểm tra Rust (nếu chưa có: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)
rustc --version
cargo --version
```

### 2. Clone Repository & Build Install
```bash
git clone git@github.com:tdnhathuy/agy-mult-profiles.git
cd agy-mult-profiles

# Build release và copy binary vào ~/.local/bin/agyp
make install
```

Đảm bảo `~/.local/bin` đã có trong `$PATH` (thêm vào `~/.zshrc` nếu chưa có):
```bash
export PATH="$HOME/.local/bin:$PATH"
```

Kiểm tra:
```bash
agyp --version
```

---

## 🔐 Đăng nhập các Profile (User làm một lần)

### 1. Import tài khoản hiện tại làm Profile `default`
Nếu trên máy đã đăng nhập sẵn 1 tài khoản `agy`:
```bash
agyp login default --import-current
```

### 2. Đăng nhập thêm Profile mới (Profile 1, Profile 2,...)
Chạy lệnh đăng nhập cho từng profile:
```bash
agyp login profile-1
```
👉 `agyp` sẽ tự động mở trình duyệt với trang Google OAuth. Hãy chọn tài khoản Google tương ứng và bấm cho phép.

Tiếp tục với tài khoản tiếp theo:
```bash
agyp login profile-2
```

### 3. Đặt Alias cho dễ gọi
Gán các alias ngắn gọn để Hermes hoặc bạn dễ gọi:
```bash
agyp alias set p1 profile-1
agyp alias set p2 profile-2
```

### 4. Kiểm tra trạng thái
```bash
agyp list
```
Kết quả hiển thị:
```text
PROFILE         EMAIL                          ALIASES         DEFAULT    STATUS         
------------------------------------------------------------------------------------------
default         user1@gmail.com                -               yes (*)    ready          
profile-1       user2@gmail.com                p1              no         ready          
profile-2       user3@gmail.com                p2              no         ready          
```

---

## 🤖 Hướng dẫn tích hợp cho Hermes (AI Agent)

Hermes có thể tương tác với `agyp` qua terminal/bash tool.

### 1. Cài đặt Agent Skill cho Hermes
File skill mẫu đã có sẵn tại `skills/agyp/SKILL.md`. Copy vào thư mục skill toàn cục của hệ thống:
```bash
mkdir -p ~/.gemini/antigravity/skills/agyp
cp skills/agyp/SKILL.md ~/.gemini/antigravity/skills/agyp/
```

### 2. System Prompt / Instruction mẫu cho Hermes
Copy đoạn sau vào System Prompt hoặc Custom Instructions của Hermes:

```markdown
You have access to `agyp` (Antigravity Multi-Profile CLI).
Use `agyp` whenever you need to execute prompts or tasks via Google Antigravity using specific accounts.

Rules for Hermes:
1. To see available profiles and their statuses:
   `agyp list --json`
2. To prompt a specific profile/account:
   `agyp prompt <profile_or_alias> "<instruction>"`
   Example:
   `agyp prompt p1 "Write a python script to parse CSV"`
3. To pipe context or large files:
   `cat file.py | agyp prompt p2 "Analyze bugs in this code"`
4. To receive machine-parseable JSON response:
   `agyp prompt p1 --json "Check status"`
5. If a command returns exit code 2 (unauthenticated):
   Notify the user to run `agyp login <profile>` in an interactive terminal.
6. Rate Limit Handling:
   If the selected profile encounters Resource Exhausted / 429, keep it pinned, inspect worktree state before any retry, and report the blocker. Never switch profiles automatically.
```

---

## 📖 CLI Commands Reference

| Lệnh | Mô tả |
| :--- | :--- |
| `agyp login <name>` | Đăng nhập tài khoản mới qua trình duyệt OAuth |
| `agyp login <name> --import-current` | Import phiên đăng nhập hiện tại từ macOS Keychain |
| `agyp list` / `agyp list --json` | Liệt kê các profile, email và alias |
| `agyp default [name]` | Xem hoặc đổi profile mặc định |
| `agyp prompt <profile> "<text>"` | Gửi prompt đến profile (tự động bypass permission) |
| `agyp prompt <profile> --json "<text>"` | Nhận kết quả dạng JSON có cấu trúc |
| `agyp prompt <profile> --model <model>` | Chọn model (`gemini-2.5-flash`,...) |
| `agyp alias set <alias> <profile>` | Gán alias cho profile |
| `agyp alias rm <alias>` | Xóa alias |
| `agyp alias list` | Liệt kê các alias |
| `agyp rm <name> [-y]` | Xóa profile và dữ liệu của profile |
| `agyp exec <profile> -- <args...>` | Chuyển tiếp tham số tùy ý cho `agy` |

---

## 🔒 Security & Credential Isolation

- Mỗi profile lưu token riêng tại `~/.gemini-profiles/profiles/<name>/keychain_token.json`.
- Khi thực thi lệnh (`prompt` / `exec`), `agyp` dùng cơ chế **RAII `KeychainGuard`**:
  1. Tạm swap token của profile đó vào macOS Keychain.
  2. Khởi chạy `agy`.
  3. Bắt bất kỳ token refresh nào ghi lại vào file của profile.
  4. Phục hồi nguyên trạng Keychain ban đầu ngay khi kết thúc.
- Ngăn chặn hoàn toàn việc rò rỉ phiên giữa các tài khoản hoặc xung đột Keychain trên macOS.
