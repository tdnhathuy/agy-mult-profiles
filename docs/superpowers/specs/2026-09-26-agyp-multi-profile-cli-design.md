# Antigravity Multi-Profile CLI (`agyp`) Design Specification

**Date:** 2026-09-26  
**Status:** Approved  
**Binary Name:** `agyp` (Antigravity Profiles)  
**Implementation Language:** Rust (`cargo`)  

---

## 1. Overview & Objectives

### 1.1 Objective
Build a lightweight, zero-dependency, ultra-fast CLI tool (`agyp`) in Rust that enables developers and autonomous AI Agents (such as Hermes or Antigravity subagents) to manage, switch, and execute commands across multiple Google Antigravity (`agy`) accounts and profiles without manual environment swapping or session collisions.

### 1.2 Core Requirements
1. **Multi-Account Profile Isolation**: Each profile possesses its own isolated credentials, OAuth tokens, and conversation history.
2. **Concurrency Safety**: Multiple agents or background processes can invoke different profiles simultaneously without file locks, race conditions, or session cross-talk.
3. **Agent-First Ergonomics**:
   - Clean, unpolluted standard output for non-interactive prompt execution.
   - Built-in permission bypass flag (`--dangerously-skip-permissions` by default in prompt mode) to ensure agents are never blocked on interactive confirmations.
   - Predictable POSIX exit codes and optional JSON formatted outputs.
   - Full support for piping input via `stdin`.
4. **Agent Skill Support**: Provision of an Antigravity/Hermes `SKILL.md` file guiding AI agents on profile discovery, selection, and prompt execution.

---

## 2. Architecture & Technical Decision

### 2.1 Native Directory Isolation (`--gemini_dir`)
Investigation and reverse-engineering of the `agy` binary (`/Users/tdnhathuy/.local/bin/agy`) revealed a native flag:
```bash
agy --gemini_dir=<path_to_directory> ...
```
When `--gemini_dir` is passed:
- `agy` stores and reads all credentials (`oauth_creds.json`, `google_accounts.json`), caches, configuration, and conversation history inside the targeted directory rather than `~/.gemini`.
- This ensures 100% native isolation, zero dependency on shell hooks or environment variables, and complete concurrency safety.

### 2.2 Directory Layout
All profile data is organized under `~/.gemini-profiles/`:
```
~/.gemini-profiles/
├── profiles.json                   # Central registry (profiles, metadata, aliases, default)
└── profiles/
    ├── work/                       # Profile: work
    │   ├── oauth_creds.json
    │   ├── google_accounts.json
    │   ├── config/
    │   └── antigravity-cli/
    └── personal/                   # Profile: personal
        ├── oauth_creds.json
        ├── google_accounts.json
        ├── config/
        └── antigravity-cli/
```

### 2.3 Registry Schema (`~/.gemini-profiles/profiles.json`)
```json
{
  "version": 1,
  "default_profile": "work",
  "aliases": {
    "p1": "work",
    "p2": "personal",
    "main": "work"
  },
  "profiles": {
    "work": {
      "name": "work",
      "email": "user.work@example.com",
      "dir": "/Users/tdnhathuy/.gemini-profiles/profiles/work",
      "created_at": "2026-09-26T22:00:00Z",
      "last_used_at": "2026-09-26T22:15:00Z"
    },
    "personal": {
      "name": "personal",
      "email": "user.personal@gmail.com",
      "dir": "/Users/tdnhathuy/.gemini-profiles/profiles/personal",
      "created_at": "2026-09-26T22:05:00Z",
      "last_used_at": "2026-09-26T22:10:00Z"
    }
  }
}
```

---

## 3. CLI Command Specifications

The CLI executable is named `agyp`.

### 3.1 Authentication & Profile Management

#### `agyp login <profile> [--import-current]`
- Initializes directory `~/.gemini-profiles/profiles/<profile>/` if it does not exist.
- If `--import-current` is specified: Copies existing `oauth_creds.json` and `google_accounts.json` from `~/.gemini` into the profile directory immediately without browser authentication.
- Otherwise: Spawns `agy --gemini_dir=<path>` interactively to allow standard browser-based Google OAuth flow.
- After exit: Reads `google_accounts.json` from the profile directory to extract the authenticated email address and records it in `profiles.json`.

#### `agyp list [--json]` (alias: `agyp ls`)
- In human mode: Renders an ASCII table showing Profile Name, Email, Aliases, Default status, and Last Used timestamp.
- In `--json` mode: Outputs valid JSON for automated machine ingestion.

#### `agyp default [profile]`
- If `profile` is provided: Updates `default_profile` in `profiles.json`.
- If omitted: Prints the currently active default profile.

#### `agyp rm <profile> [-y|--yes]`
- Removes the specified profile directory and removes any associated aliases.
- Requires confirmation unless `-y` is provided.

---

### 3.2 Alias Management

#### `agyp alias set <alias> <profile>`
- Associates `<alias>` with an existing `<profile>`.
- Validates that the target profile exists.

#### `agyp alias rm <alias>`
- Removes an existing alias.

#### `agyp alias list [--json]`
- Lists all active aliases and their target profiles.

---

### 3.3 Prompting & Execution (Agent-Optimized)

#### `agyp prompt <profile_or_alias> [prompt_text] [options]` (alias: `agyp -p`)
Designed for non-interactive LLM prompts by AI agents.
- **Profile Resolution**: Resolves alias or profile name; if omitted, falls back to `default_profile`.
- **Options**:
  - `--model <model>`: Forwards model override to `agy` (e.g. `--model gemini-2.5-flash`).
  - `--add-dir <dir>`: Forwards directory attachment to `agy`.
  - `--no-skip-perm`: Disables automatic `--dangerously-skip-permissions` (which is enabled by default).
  - `--json`: Formats wrapper status and raw stdout as JSON:
    ```json
    {
      "profile": "work",
      "email": "user.work@example.com",
      "status": "success",
      "exit_code": 0,
      "output": "..."
    }
    ```
- **Piping Support**: If `prompt_text` is omitted, reads prompt from `stdin`:
  ```bash
  cat report.md | agyp prompt p1 "Summarize this file"
  ```
- **Stream Output**: Directly streams stdout and stderr from `agy` to caller process.

#### `agyp exec <profile_or_alias> -- <agy_args...>`
Runs arbitrary `agy` arguments directly under the specified profile:
```bash
agyp exec p1 -- --continue
agyp exec p2 -- -i "Start new feature"
```

---

## 4. Execution Flow & Signal Handling

```
+-------------------------------------------------------------+
|                     Caller (Hermes / Shell)                 |
|            agyp prompt p1 "Check database schema"           |
+-------------------------------------------------------------+
                              |
                              v
+-------------------------------------------------------------+
|                        agyp binary                          |
| 1. Read ~/.gemini-profiles/profiles.json                    |
| 2. Resolve alias "p1" -> "work" -> ~/.gemini-profiles/...   |
| 3. Verify directory & credentials exist                     |
| 4. Update last_used_at in profiles.json                     |
| 5. Locate agy binary (~/.local/bin/agy or $PATH)            |
| 6. Assemble Command:                                        |
|    agy --gemini_dir=<path> --dangerously-skip-permissions   |
|        -p "Check database schema"                           |
| 7. Forward SIGINT/SIGTERM to child process                  |
+-------------------------------------------------------------+
                              |
                              v
+-------------------------------------------------------------+
|                         agy process                         |
| Runs prompt with isolated credentials and prints response   |
+-------------------------------------------------------------+
```

---

## 5. Error Handling & Exit Codes

| Exit Code | Cause | Behavior |
|:---|:---|:---|
| `0` | Success | Output returned cleanly. |
| `1` | Invalid Usage / Profile Not Found | Clear error message listing available profiles. |
| `2` | Profile Not Authenticated | Notice indicating credentials missing or expired; suggests `agyp login <profile>`. |
| `127` | `agy` Not Found | Error informing user that `agy` binary was not found in PATH or standard locations. |
| Other | `agy` Failure | Exact exit code from `agy` process is bubbled up directly to caller. |

---

## 6. AI Agent Skill Specification (`skills/agyp/SKILL.md`)

A dedicated Skill will be installed to guide Hermes and other AI agents:
- **Trigger**: When the agent needs to query or automate tasks across multiple Antigravity profiles.
- **Workflow**:
  1. Inspect available profiles: `agyp list --json`
  2. Select profile based on project requirements (e.g. `work` vs `personal`).
  3. Execute prompt: `agyp prompt <profile> "<instruction>"`
  4. Parse output and handle failures if authentication is needed.

---

## 7. Testing & Verification Plan

1. **Unit & CLI Tests**:
   - Profile resolution and alias mapping.
   - Argument parsing and flag forwarding.
   - Non-existent profile error handling.
2. **Integration Verification**:
   - `agyp login p1 --import-current`: Verify current session imports cleanly.
   - `agyp prompt p1 "ping"`: Verify response receives "pong" or valid model response.
   - `agyp list`: Verify JSON and table rendering.
   - `agyp alias set / rm`: Verify alias updates in `profiles.json`.
3. **Concurrent Execution Verification**:
   - Spawn two parallel calls (`agyp prompt p1` and `agyp prompt p2`) concurrently to ensure zero conflict.
