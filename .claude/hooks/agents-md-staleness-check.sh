#!/usr/bin/env bash
# PostToolUse hook for `Write` / `Edit`. Reads the hook payload from stdin and
# prints a one-line reminder to stderr when the agent edited a structural file
# without also touching AGENTS.md. The reminder shows up in the agent's
# next-turn context so it can decide whether to update the docs.
#
# Exits 0 always — this hook never blocks. The agent makes the judgment call.

set -uo pipefail

payload=$(cat 2>/dev/null || true)
[ -z "$payload" ] && exit 0

# Need jq to read the edited path out of the payload. If absent, skip silently
# rather than blocking edits.
if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

file=$(printf '%s' "$payload" \
  | jq -r '.tool_response.filePath // .tool_input.file_path // empty' 2>/dev/null)
[ -z "$file" ] && exit 0

project="${CLAUDE_PROJECT_DIR:-$PWD}"
# Compute repo-relative path so glob matches work regardless of cwd.
case "$file" in
  /*) rel="${file#"$project"/}" ;;
  *)  rel="$file" ;;
esac

# AGENTS.md edits never need a reminder about themselves — but they do get a
# size check. The file loads into every session and every subagent, so bytes
# here are paid on every turn; the contract stays short and the rationale lives
# in docs/design/. A reminder without a number did not hold (the file grew 5x
# in ten weeks after its last trim), hence the budget.
AGENTS_MD_BUDGET_BYTES=${AGENTS_MD_BUDGET_BYTES:-30000}
case "$rel" in
  AGENTS.md)
    project="${CLAUDE_PROJECT_DIR:-$PWD}"
    size=$(wc -c < "$project/AGENTS.md" 2>/dev/null | tr -d ' ')
    if [ -n "$size" ] && [ "$size" -gt "$AGENTS_MD_BUDGET_BYTES" ]; then
      printf '[agents-md-check] AGENTS.md is %s bytes, over the %s-byte budget. Keep the rule here and move the rationale to docs/design/.\n' "$size" "$AGENTS_MD_BUDGET_BYTES" >&2
    fi
    exit 0
    ;;
  CLAUDE.md) exit 0 ;;
esac

# Structural surfaces. Editing any of these is the trigger for "consider
# updating AGENTS.md". Keep this list in sync with the "Keeping this file up to
# date" section of AGENTS.md.
match=0
case "$rel" in
  rust-toolchain.toml) match=1 ;;
  .claude/settings.json) match=1 ;;
  justfile) match=1 ;;
  src-tauri/Cargo.toml) match=1 ;;
  src-tauri/Cargo.lock) match=1 ;;
  web-rs/Cargo.toml) match=1 ;;
  web-rs/Cargo.lock) match=1 ;;
  src-tauri/tauri.conf.json) match=1 ;;
  src-tauri/build.rs) match=1 ;;
  src-tauri/capabilities/*) match=1 ;;
  web-rs/Trunk.toml) match=1 ;;
  .github/workflows/*.yml|.github/workflows/*.yaml) match=1 ;;
esac
[ "$match" -eq 0 ] && exit 0

# If AGENTS.md is already modified in the working tree (staged or unstaged),
# the agent is already updating it — stay silent.
cd "$project" 2>/dev/null || exit 0
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if git status --porcelain -- AGENTS.md 2>/dev/null | grep -q .; then
    exit 0
  fi
fi

printf '[agents-md-check] Edited %s. If this affects repo map / commands / conventions, update AGENTS.md in this change.\n' "$rel" >&2
exit 0
