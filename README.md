# agyp - Antigravity Multi-Profile CLI

CLI tool written in Rust to manage multiple Google Antigravity (`agy`) accounts and profiles with full credential isolation on macOS. Specifically designed for **Developers** and **AI Agents (such as Hermes)** to execute isolated prompts across multiple accounts non-interactively.

---

## ⚡ Highlights

- **True macOS Keychain Isolation**: Solves `agy`'s global Keychain auto-login collision by giving every profile its own isolated HOME and private Keychain (`~/.gemini-profiles/homes/<name>/`).
- **Parallel Profiles**: Different profiles run concurrently (per-profile locks); the shared system Keychain is never swapped.
- **Seamless Browser OAuth**: Login runs inside the profile's isolated HOME, so Google displays the account selection screen without touching the system Keychain.
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
2. Check quota availability before assigning tasks:
   `agyp quota --all --json`
   (Note: Uses a minimal observed-health probe; cached for 5m; `--timeout` is one total deadline for the whole sequential run; never rotates profiles automatically on 429).
   For a quick Gemini-only percentage check with Vietnam reset times:
   `agyp q` (all profiles) or `agyp q <profile>`. Output includes each profile's email. Each profile has a 60-second transaction deadline; timeouts are reported without automatic retry or profile switching.
3. For coding tasks, ALWAYS use `agyp task` in an isolated Git linked worktree:
   `agyp task --profile <profile> --worktree <worktree-path> --prompt-file <path> --timeout <duration> --json`
   Or pipe prompt via stdin:
   `cat prompt.txt | agyp task --profile <profile> --worktree <worktree-path> --timeout <duration> --json`
   `agyp task` strictly isolates credentials, validates the linked worktree, drains streams concurrently to avoid deadlocks, enforces execution deadlines, and produces structured JSON output.
3. Use `agyp prompt <profile> "<instruction>"` ONLY for non-coding or basic interactive use.
   Do NOT use `agyp prompt` for coding delegation: it lacks linked-worktree isolation, stream deadlocking protections, deadline timeouts, and structured failure classification.
4. If a command returns exit code 2 (unauthenticated):
   Notify the user to run `agyp login <profile>` in an interactive terminal.
5. Rate Limit Handling:
   If the selected profile encounters Resource Exhausted / 429, keep it pinned, inspect worktree state before any retry, and report the blocker. Never switch profiles automatically.
```

---

## 📖 CLI Commands Reference

| Lệnh | Mô tả |
| :--- | :--- |
| `agyp q` / `agyp quota --all` | Quota Gemini + GPT: %, thanh usage, còn bao lâu. `--hide-gpt` ẩn GPT |
| `agyp quota --profile <p>` / `--all` | Log quota Gemini + GPT (profile, email, 5 giờ, 5 ngày, reset). `--json` vẫn là probe |
| `agyp task --profile <p> --worktree <path>` | Chạy task tự động trong Git linked worktree với stream draining, timeout & JSON classification |
| `agyp login <name>` | Đăng nhập tài khoản mới qua trình duyệt OAuth |
| `agyp login <name> --import-current` | Import phiên đăng nhập hiện tại từ macOS Keychain |
| `agyp list` / `agyp list --json` | Liệt kê các profile, email và alias |
| `agyp default [name]` | Xem hoặc đổi profile mặc định |
| `agyp prompt <profile> "<text>"` | Gửi prompt đến profile |
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
- `agy` chỉ đọc token từ **một** item Keychain cố định (`service=gemini`, `account=antigravity`) qua `/usr/bin/security`, và `security` chọn Keychain dựa trên `$HOME`. Vì vậy mỗi profile có **HOME + Keychain riêng** tại `~/.gemini-profiles/homes/<name>/`:
  - `Library/Keychains/login.keychain-db`: Keychain riêng của profile (mật khẩu ngẫu nhiên lưu trong `profiles/<name>/keychain_password`, không auto-lock).
  - Các mục còn lại của HOME thật (`.gitconfig`, `.ssh`, `.config`, `.gemini`, `Library/*`, ...) được symlink để git/npm/... trong task vẫn hoạt động. Riêng `Library/Keychains` và `Library/Preferences` là thư mục riêng.
  - Trước mỗi lần chạy, `agyp` kiểm tra search list/default Keychain dưới HOME riêng chỉ trỏ vào Keychain của profile; nếu không thì từ chối chạy.
- Keychain riêng là nơi lưu token chính của profile: lần chạy đầu nạp từ `keychain_token.json`, sau đó `agy` refresh token ngay trong Keychain riêng, và `agyp` sao lưu lại vào `keychain_token.json` sau mỗi lần chạy. Keychain hệ thống **không bị đụng tới**.
- Lock theo profile (`~/.gemini-profiles/locks/<name>.lock`) chỉ giữ vài giây lúc chuẩn bị Keychain, nên **nhiều phiên chạy song song được, kể cả trên cùng một profile**. Nếu profile đang bận, `agyp` báo `... is in use by another agyp command; waiting...`.
- `agyp login <name>` chạy OAuth bên trong HOME riêng của profile. Ngay khi OAuth xong, profile được ghi vào registry (hiện trong `agyp ls`) và dùng được luôn, dù phiên `agy` của login vẫn đang mở.
- Lưu ý: tool trong task dùng macOS Keychain (vd. `gh` lưu token trong Keychain, git HTTPS qua `osxkeychain`) sẽ không thấy credential của login Keychain thật. Dùng SSH cho git hoặc `GH_TOKEN` cho `gh`.

---

## 🧩 codexp — Codex CLI multi-account

`codexp` (cài cùng `make install`) chạy `codex exec` dưới nhiều account ChatGPT, mỗi profile một `CODEX_HOME` riêng ở `~/.codex-profiles/homes/<name>/`. Không cần Keychain, các profile chạy song song thoải mái.

**codexp không bao giờ login hay refresh token.** Account được login ở OmniRoute, export `auth.json` rồi import vào codexp:

```bash
codexp import c1 --from ~/Desktop/auth-codex/auth-a@gmail.com.json --remove-source
codexp list
codexp task --profile c1 --worktree ../wt-a --prompt-file prompt.md --timeout 30m --json
codexp q                  # quota Codex của mọi profile (5 giờ + 7 ngày, giờ VN)
codexp exec c1            # mở Codex TUI bằng account c1
```

- Khi import, refresh token bị thay bằng giá trị giả và endpoint refresh/revoke của Codex bị trỏ vào địa chỉ chết, nên codexp không thể làm xoay (rotate) hay thu hồi token đang dùng ở OmniRoute.
- Đổi lại, khi access token hết hạn (~7–10 ngày) profile sẽ báo `unauthenticated` (exit 2): export lại từ OmniRoute rồi `codexp import <name> --from <file>` (cùng account thì ghi đè bình thường).
- Import từ chối file có `id_token` / `access_token` / `account_id` không cùng một account, file đã hết hạn, và việc đổi account trong cùng profile hoặc import trùng account (trừ khi `--force`).
- Config Codex dùng chung: `~/.codex-profiles/config.toml`. codexp luôn ép `model_provider="openai"`, `cli_auth_credentials_store="file"`, `forced_login_method="chatgpt"`. `AGENTS.md`, `skills`, `rules` được symlink từ `~/.codex`.
- `codexp q [profile] [--json]` gọi endpoint usage của ChatGPT (cùng nguồn với `/status` của Codex) bằng access token đã import: chỉ đọc, không refresh. Gói plus có cửa sổ 5 giờ và 7 ngày, gói free có một cửa sổ 30 ngày.
- Skill cho orchestrator: `skills/codexp/SKILL.md`.
