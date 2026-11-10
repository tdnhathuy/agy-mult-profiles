#!/usr/bin/env bash
# Run one browser task on an AGY (Gemini) worker that drives Orca's built-in browser via the orca CLI.
#
# Usage: agy-web.sh <spec-file> [profile]
#   spec-file  Task spec written from the agy-web template (absolute or relative path).
#   profile    agyp profile; default $AGY_WEB_PROFILE, else the available profile with the most Gemini quota.
# Env: AGY_WEB_MODEL (default gemini-3.8-flash-high).
#
# Run it from the checkout whose Orca browser tabs the task should use: orca browser commands
# target the worktree of the current directory. Prints the worker's report; also saves it next
# to the spec as <spec>.out.txt. Exits non-zero when agyp fails or the report has no WEB_TASK_DONE.
set -euo pipefail

[[ $# -ge 1 && -f "$1" ]] || { echo "Usage: agy-web.sh <spec-file> [profile]" >&2; exit 2; }
spec="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
[[ "$spec" != *" "* ]] || { echo "Spec path must not contain spaces: $spec" >&2; exit 2; }
profile="${2:-${AGY_WEB_PROFILE:-}}"
model="${AGY_WEB_MODEL:-gemini-3.8-flash-high}"

if [[ -z "$profile" ]]; then
  profile="$(agyp q --hide-gpt --json 2>/dev/null | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)
best = None
for r in data.get("results", []):
    if r.get("status") != "available":
        continue
    g = r.get("gemini") or {}
    pcts = [(g.get(w) or {}).get("remaining_percent") for w in ("five_hour", "five_day")]
    if any(p is None or p <= 0 for p in pcts):
        continue
    score = min(pcts)
    if best is None or score > best[0]:
        best = (score, r["profile"])
if best:
    print(best[1])
' || true)"
  [[ -n "$profile" ]] || profile="$(agyp default 2>/dev/null | head -n 1)"
fi
[[ -n "$profile" ]] || { echo "No agyp profile available (quota exhausted?)." >&2; exit 3; }

out_file="${spec%.*}.out.txt"
raw="$(agyp prompt "$profile" --skip-permissions --json --model "$model" \
  "Read $spec and follow every instruction in it." 2>&1)" || true

python3 - "$profile" "$out_file" "$raw" <<'PY'
import json, sys
profile, out_file, raw = sys.argv[1], sys.argv[2], sys.argv[3]
start = raw.find("{")
try:
    data = json.loads(raw[start:]) if start >= 0 else {}
except json.JSONDecodeError:
    data = {}
output = (data.get("output") or "").strip()
status = data.get("status")
report = f"PROFILE: {profile}\nSTATUS: {status}\n{output or raw.strip()}\n"
open(out_file, "w", encoding="utf-8").write(report)
print(report, end="")
if status != "success":
    sys.exit(4)
if "WEB_TASK_DONE" not in output:
    print("agy-web: report has no WEB_TASK_DONE line; treat the task as unfinished.", file=sys.stderr)
    sys.exit(5)
PY
