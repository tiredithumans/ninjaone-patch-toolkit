#!/usr/bin/env bash
# PostToolUse hook for `Write` / `Edit`. Reads the hook payload from stdin and
# prints a one-line reminder to stderr when the agent edited a user-facing
# surface (Tauri commands, packaging config, build scripts, the auth flow, …)
# without also touching the matching prose doc (README.md). The reminder shows
# up in the agent's next-turn context so it can decide whether to update README.
#
# This is the README counterpart to `agents-md-staleness-check.sh`. AGENTS.md is
# the agent contract; the README goes stale a different way (a new command →
# Features gap, a tauri.conf.json change → packaging-doc gap, etc.).
#
# Exits 0 always — this hook never blocks. The agent makes the judgment call.

set -uo pipefail

payload=$(cat 2>/dev/null || true)
[ -z "$payload" ] && exit 0

if ! command -v jq >/dev/null 2>&1; then
  exit 0
fi

file=$(printf '%s' "$payload" \
  | jq -r '.tool_response.filePath // .tool_input.file_path // empty' 2>/dev/null)
[ -z "$file" ] && exit 0

project="${CLAUDE_PROJECT_DIR:-$PWD}"
case "$file" in
  /*) rel="${file#"$project"/}" ;;
  *)  rel="$file" ;;
esac

# Doc files themselves and the agent-contract files don't trigger a docs
# reminder — AGENTS.md has its own hook.
case "$rel" in
  README.md|AGENTS.md|CLAUDE.md) exit 0 ;;
  docs/*) exit 0 ;;
esac

# Skip tests, generated artifacts, and lockfiles. Editing these almost never
# requires a prose-doc update.
case "$rel" in
  *_test.rs|*/tests/*|tests/*) exit 0 ;;
  target/*|*/target/*) exit 0 ;;
  dist/*|*/dist/*) exit 0 ;;
  *.lock) exit 0 ;;
esac

# Map the edited path to the README section most likely to lag behind. Order
# matters: the first match wins, so put narrower patterns ahead of broader ones.
# The hint string is the surface the agent should re-skim — it's a suggestion,
# not an assertion that the doc is wrong.
hint=""
case "$rel" in
  src-tauri/src/commands/*|src-tauri/src/export.rs|src-tauri/src/filter.rs|src-tauri/src/rows.rs)
    hint="README.md (Features)"
    ;;
  src-tauri/src/auth.rs)
    hint="README.md (NinjaOne setup / Security)"
    ;;
  src-tauri/tauri.conf.json|src-tauri/build.rs)
    hint="README.md (Build & verify)"
    ;;
  justfile)
    hint="README.md (Run / Build & verify)"
    ;;
  .github/workflows/*)
    hint="README.md (Build & verify)"
    ;;
  src-tauri/Cargo.toml|web-rs/Cargo.toml|rust-toolchain.toml)
    hint="README.md (Prerequisites)"
    ;;
  *)
    exit 0
    ;;
esac

cd "$project" 2>/dev/null || exit 0

# Stay silent if README.md is already modified — the agent is already updating
# prose.
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if git status --porcelain -- README.md 2>/dev/null | grep -q .; then
    exit 0
  fi
fi

printf '[docs-check] Edited %s. If user-facing behavior changed, re-skim %s.\n' \
  "$rel" "$hint" >&2
exit 0
