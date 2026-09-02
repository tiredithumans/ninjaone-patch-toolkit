#!/usr/bin/env bash
# Self-test for the hooks in this directory. Pipes fixed Claude Code payloads
# through each hook and asserts on what it prints, so a hook that drifts from the
# code it inspects (the way the parity check did when the `ipc!` macro replaced
# the quoted invoke strings) fails here instead of nagging every session.
#
#   .claude/hooks/test.sh          # run from anywhere inside the repo
#
# Exit 1 if any assertion failed. No dependencies beyond what the hooks
# themselves need (bash, jq, python3, git).

set -uo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
project=$(cd "$here/../.." && pwd)
export CLAUDE_PROJECT_DIR="$project"

fail=0
pass=0

# run_hook <hook> <payload-json> → stderr captured in $out, exit code in $code
run_hook() {
  out=$(printf '%s' "$2" | "$here/$1" 2>&1 >/dev/null)
  code=$?
}

expect_silent() { # <label>
  if [ -n "$out" ]; then
    printf 'FAIL %s: expected no output, got:\n%s\n' "$1" "$out"; fail=$((fail + 1))
  else
    pass=$((pass + 1))
  fi
}

expect_contains() { # <label> <needle>
  if printf '%s' "$out" | grep -qF -- "$2"; then
    pass=$((pass + 1))
  else
    printf 'FAIL %s: expected output containing %s, got:\n%s\n' "$1" "$2" "$out"; fail=$((fail + 1))
  fi
}

expect_code() { # <label> <code>
  if [ "$code" -eq "$2" ]; then
    pass=$((pass + 1))
  else
    printf 'FAIL %s: expected exit %s, got %s (output: %s)\n' "$1" "$2" "$code" "$out"; fail=$((fail + 1))
  fi
}

edit_payload() { # <repo-relative path>
  printf '{"tool_input":{"file_path":"%s/%s"},"tool_response":{"filePath":"%s/%s"}}' \
    "$project" "$1" "$project" "$1"
}

bash_payload() { # <command>
  python3 -c 'import json,sys; print(json.dumps({"tool_input":{"command":sys.argv[1]}}))' "$1"
}

# --- command-parity-check ----------------------------------------------------
# On the real tree every declared command has an ipc! wrapper, so an edit inside
# the command chain must be silent. This is the assertion that was false for
# months: the hook looked for `"name"` literals the macro no longer emits.
run_hook command-parity-check.sh "$(edit_payload src-tauri/src/commands/patches.rs)"
expect_silent "parity: clean tree is silent"

run_hook command-parity-check.sh "$(edit_payload web-rs/src/app/tables.rs)"
expect_silent "parity: unrelated file is silent"

# A command with no wrapper must be named. Simulate by pointing the hook at a
# scratch copy of the tree with one wrapper removed.
tmp=$(mktemp -d)
mkdir -p "$tmp/src-tauri/src/commands" "$tmp/web-rs/src"
cp "$project"/src-tauri/src/commands/*.rs "$tmp/src-tauri/src/commands/"
cp "$project/src-tauri/src/lib.rs" "$tmp/src-tauri/src/lib.rs"
grep -v 'list_node_classes' "$project/web-rs/src/api.rs" > "$tmp/web-rs/src/api.rs"
CLAUDE_PROJECT_DIR="$tmp" run_hook command-parity-check.sh \
  "$(printf '{"tool_input":{"file_path":"%s/src-tauri/src/lib.rs"}}' "$tmp")"
# The hook backquotes the command name; build the needle so the backticks stay
# literal without shellcheck reading them as a command substitution.
bt='\140'
expect_contains "parity: missing wrapper is reported" \
  "$(printf "%blist_node_classes%b has no ipc! wrapper" "$bt" "$bt")"
rm -rf "$tmp"

# --- agents-md-staleness-check -----------------------------------------------
run_hook agents-md-staleness-check.sh "$(edit_payload src-tauri/src/rows/mod.rs)"
expect_silent "agents-md: source edit is silent"

if git -C "$project" status --porcelain -- AGENTS.md | grep -q .; then
  echo "skip agents-md: AGENTS.md is modified in the working tree, structural reminder suppressed"
else
  run_hook agents-md-staleness-check.sh "$(edit_payload justfile)"
  expect_contains "agents-md: structural edit reminds" 'update AGENTS.md'
fi

AGENTS_MD_BUDGET_BYTES=1 run_hook agents-md-staleness-check.sh "$(edit_payload AGENTS.md)"
expect_contains "agents-md: over-budget warns" 'over the 1-byte budget'

run_hook agents-md-staleness-check.sh "$(edit_payload AGENTS.md)"
expect_silent "agents-md: within budget is silent"

# --- docs-staleness-check ----------------------------------------------------
run_hook docs-staleness-check.sh "$(edit_payload src-tauri/src/commands/patches.rs)"
expect_silent "docs: internal source edit is silent"

if git -C "$project" status --porcelain -- README.md | grep -q .; then
  echo "skip docs: README.md is modified in the working tree, reminder suppressed"
else
  run_hook docs-staleness-check.sh "$(edit_payload src-tauri/tauri.conf.json)"
  expect_contains "docs: packaging edit reminds" 'Build & verify'
fi

# --- conventional-commit-validator -------------------------------------------
run_hook conventional-commit-validator.sh "$(bash_payload 'ls -la')"
expect_code "validator: non-commit passes" 0

run_hook conventional-commit-validator.sh "$(bash_payload 'git commit -m "fix(web): keep Tab inside the dialog"')"
expect_code "validator: conventional subject passes" 0

run_hook conventional-commit-validator.sh "$(bash_payload 'git commit -m "Fixed the dialog"')"
expect_code "validator: bad subject is blocked" 2
expect_contains "validator: bad subject names the rule" 'Conventional Commits'

heredoc=$(cat <<'MSG'
git commit -m "$(cat <<'EOF'
docs: explain the "why" in design notes

Body with a "quoted" word.
EOF
)"
MSG
)
run_hook conventional-commit-validator.sh "$(bash_payload "$heredoc")"
expect_code "validator: heredoc form with quotes passes" 0

printf '%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
