#!/usr/bin/env bash
# Install this repo's agent skills (skills/<name>/SKILL.md) globally so every agent sees them:
#   ~/.agents/skills             Codex (GPT) and other agents that read the shared skills dir
#   ~/.claude/skills             Claude Code
#   ~/.gemini/antigravity/skills Antigravity (AGY) / Hermes
#
# Default mode symlinks each skill back to this repo, so `git pull` updates every agent at once.
# An existing non-symlink skill at a destination is moved to <dest>.bak-<timestamp>, never deleted.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKILLS_DIR="$REPO_DIR/skills"
MARKER=".installed-from-agy-mult-profiles"

usage() {
  cat <<EOF
Usage: $(basename "$0") [options] [skill...]

Install skills from $SKILLS_DIR into global agent skill directories.
With no skill names, installs every skill in the repo ($(ls "$SKILLS_DIR" | tr '\n' ' '| sed 's/ $//')).

Options:
  --copy          Copy files instead of symlinking (re-run after every update)
  --uninstall     Remove skills previously installed by this script
  --target DIR    Install into DIR instead of the defaults (repeatable)
  --dry-run       Print what would happen without changing anything
  -h, --help      Show this help

Default targets (skipped when the agent is not installed):
  ~/.agents/skills              Codex / GPT and other agents (always created)
  ~/.claude/skills              Claude Code (needs ~/.claude)
  ~/.gemini/antigravity/skills  Antigravity / AGY (needs ~/.gemini/antigravity)

Examples:
  $(basename "$0")                 # symlink all skills into every agent
  $(basename "$0") agyp            # only the agyp skill
  $(basename "$0") --copy          # standalone copies (no dependency on this checkout)
  $(basename "$0") --uninstall     # remove them again
EOF
}

MODE=link
DRY_RUN=0
TARGETS=()
SKILLS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --copy) MODE=copy ;;
    --uninstall) MODE=uninstall ;;
    --dry-run) DRY_RUN=1 ;;
    --target)
      [ $# -ge 2 ] || { echo "error: --target needs a directory" >&2; exit 1; }
      TARGETS+=("$2"); shift ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "error: unknown option $1 (see --help)" >&2; exit 1 ;;
    *) SKILLS+=("$1") ;;
  esac
  shift
done

# Show paths under $HOME as ~/... in messages.
pretty() {
  case "$1" in
    "$HOME"/*) echo "~${1#"$HOME"}" ;;
    *) echo "$1" ;;
  esac
}

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    echo "  [dry-run] $*"
  else
    "$@"
  fi
}

if [ ${#SKILLS[@]} -eq 0 ]; then
  for dir in "$SKILLS_DIR"/*/; do
    [ -f "$dir/SKILL.md" ] && SKILLS+=("$(basename "$dir")")
  done
fi
for skill in "${SKILLS[@]}"; do
  if [ ! -f "$SKILLS_DIR/$skill/SKILL.md" ]; then
    echo "error: unknown skill '$skill' (no $SKILLS_DIR/$skill/SKILL.md)" >&2
    exit 1
  fi
done

if [ ${#TARGETS[@]} -eq 0 ]; then
  TARGETS+=("$HOME/.agents/skills")
  if [ -d "$HOME/.claude" ]; then
    TARGETS+=("$HOME/.claude/skills")
  else
    echo "skip: ~/.claude not found (Claude Code not installed)"
  fi
  if [ -d "$HOME/.gemini/antigravity" ]; then
    TARGETS+=("$HOME/.gemini/antigravity/skills")
  else
    echo "skip: ~/.gemini/antigravity not found (Antigravity not installed)"
  fi
fi

# True when $1 was installed by this script: a symlink into this repo, or a copy with our marker.
owned_by_us() {
  local dest="$1"
  if [ -L "$dest" ]; then
    case "$(readlink "$dest")" in
      "$SKILLS_DIR"/*) return 0 ;;
    esac
    return 1
  fi
  [ -f "$dest/$MARKER" ]
}

backup() {
  local dest="$1"
  local bak
  bak="$dest.bak-$(date +%Y%m%d%H%M%S)"
  echo "  backup: $(pretty "$dest") -> $(pretty "$bak")"
  run mv "$dest" "$bak"
}

install_one() {
  local src="$1" dest="$2"

  if [ "$MODE" = link ] && [ -L "$dest" ] && [ "$(readlink "$dest")" = "$src" ]; then
    echo "  ok: $(pretty "$dest") (already linked)"
    return
  fi
  if [ -L "$dest" ] || [ -e "$dest" ]; then
    if owned_by_us "$dest"; then
      run rm -rf "$dest"
    else
      backup "$dest"
    fi
  fi

  if [ "$MODE" = link ]; then
    run ln -s "$src" "$dest"
    echo "  linked: $(pretty "$dest") -> $(pretty "$src")"
  else
    run cp -R "$src" "$dest"
    if [ "$DRY_RUN" -eq 0 ]; then
      echo "Installed by $REPO_DIR/scripts/install-skills.sh --copy" > "$dest/$MARKER"
    fi
    echo "  copied: $(pretty "$src") -> $(pretty "$dest")"
  fi
}

uninstall_one() {
  local dest="$1"
  if [ ! -L "$dest" ] && [ ! -e "$dest" ]; then
    return
  fi
  if owned_by_us "$dest"; then
    run rm -rf "$dest"
    echo "  removed: $(pretty "$dest")"
  else
    echo "  kept: $(pretty "$dest") (not installed by this script)"
  fi
}

for target in "${TARGETS[@]}"; do
  echo "$(pretty "$target")"
  if [ "$MODE" != uninstall ] && [ ! -d "$target" ]; then
    run mkdir -p "$target"
  fi
  for skill in "${SKILLS[@]}"; do
    if [ "$MODE" = uninstall ]; then
      uninstall_one "$target/$skill"
    else
      install_one "$SKILLS_DIR/$skill" "$target/$skill"
    fi
  done
done

if [ "$MODE" != uninstall ]; then
  echo "Done. Restart running agent sessions (Claude Code, Codex, Orca workers) to load the skills."
fi
