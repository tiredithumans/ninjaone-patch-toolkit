#!/usr/bin/env bash
# PreToolUse hook for `Bash`. Reads the Claude Code hook payload from stdin,
# extracts any `git commit -m "<subject>"` invocation, and rejects subjects
# that do not match Conventional Commits.
#
# Exit codes follow Claude Code's hook contract:
#   0 — allow the Bash command (no commit / valid subject / unparseable)
#   2 — block the Bash command and surface stderr to Claude for self-correction

set -uo pipefail

payload=$(cat 2>/dev/null || true)

# Cheap pre-filter: skip the heavy parse when the command can't possibly be a
# `git commit`. This hook fires on every Bash call, so the common path must be
# fast. Deliberately loose — `git -C dir commit` and `cd x && git -c k=v commit`
# have no literal "git commit" in them.
case "$payload" in
  *git*commit*) ;;
  *) exit 0 ;;
esac

# Need python3 for reliable JSON + shell-quoted argument parsing. If absent,
# fail open — the agent still has the rule from AGENTS.md.
if ! command -v python3 >/dev/null 2>&1; then
  exit 0
fi

PAYLOAD="$payload" python3 <<'PY'
import json, os, re, shlex, sys

try:
    payload = json.loads(os.environ.get("PAYLOAD", "") or "{}")
except (json.JSONDecodeError, ValueError):
    sys.exit(0)

cmd = (payload.get("tool_input") or {}).get("command", "") or ""
if "git" not in cmd or "commit" not in cmd:
    sys.exit(0)

# git's own options that come BEFORE the subcommand and take a separate value
# (`git -C dir commit`, `git -c key=val commit`). Every other leading `-x` /
# `--long[=v]` is a flag with no separate value.
GLOBAL_OPTS_WITH_VALUE = {
    "-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env",
    "--exec-path", "--super-prefix",
}

def commit_args(tokens):
    """The argv after `commit` when `tokens` contains a `git … commit` call."""
    for i, t in enumerate(tokens):
        if t != "git" and not t.endswith("/git"):
            continue
        j = i + 1
        while j < len(tokens) and tokens[j].startswith("-"):
            opt = tokens[j]
            j += 2 if opt in GLOBAL_OPTS_WITH_VALUE else 1
        if j < len(tokens) and tokens[j] == "commit":
            return tokens[j + 1:]
    return None

# Short `git commit` flags that take no value, so they can precede an attached
# `m` in one cluster: `-am"msg"` arrives from shlex as `-ammsg`.
NO_VALUE_SHORT = "asnevqpio"

def find_subject(segment):
    if "git" not in segment or "commit" not in segment:
        return None
    try:
        tokens = shlex.split(segment, posix=True)
    except ValueError:
        return None
    args = commit_args(tokens)
    if args is None:
        return None
    j = 0
    while j < len(args):
        t = args[j]
        if t in ("-m", "--message") and j + 1 < len(args):
            return args[j + 1]
        if t.startswith("--message="):
            return t.split("=", 1)[1]
        # combined short flags ending in `m` (e.g. -am, -sm)
        if re.fullmatch(r"-[A-Za-z]*m", t) and j + 1 < len(args):
            return args[j + 1]
        # the value attached to the flag (e.g. -m"subject", -am"subject")
        attached = re.fullmatch(r"-[" + NO_VALUE_SHORT + r"]*m(.+)", t, re.DOTALL)
        if attached:
            return attached.group(1)
        j += 1
    return None

def first_nonempty_line(text):
    for line in text.splitlines():
        stripped = line.strip()
        if stripped:
            return stripped
    return None

# Unwrap the `-m "$(cat <<'TAG' ... TAG\n)"` heredoc form on the RAW command
# string, before the segment split and shlex. shlex can't see that quoting
# restarts inside $( ... ), so a double quote in the heredoc body truncates
# the -m token mid-message; and splitting on && / ; / | first could cut a
# body that contains those characters. The flag anchor keeps this from
# matching heredocs fed to other commands chained after the commit (e.g. a
# `gh pr create --body "$(cat <<'EOF' ...)"`).
heredoc = re.search(
    r"git(?:\s+(?:-[Cc]\s+\S+|--?[\w-]+(?:=\S+)?))*\s+commit\s[^\n]*?(?:-[A-Za-z]*m|--message)[= \t]*[\"']?"
    r"\$\(\s*cat\s+<<-?\s*[\"']?([A-Za-z_][A-Za-z0-9_]*)[\"']?[ \t]*\n"
    r"(.*?)\n[ \t]*\1[ \t]*(?:\n|$)",
    cmd,
    re.DOTALL,
)
subject = first_nonempty_line(heredoc.group(2)) if heredoc else None

if subject is None:
    # Approximate split on chained shell separators. Quoted separators won't
    # be split — that's acceptable: this hook backs up the prompt rule, it
    # isn't a security boundary.
    for segment in re.split(r"&&|\|\||;|\|", cmd):
        subject = find_subject(segment)
        if subject is not None:
            break

if not subject:
    sys.exit(0)

# Fallback unwrap for a heredoc token the raw-string scan above didn't match
# but shlex still captured whole (e.g. an unanchored flag spelling). The
# captured "subject" is then the literal wrapper plus body; use the heredoc
# body's first non-empty line as the real subject.
heredoc_match = re.match(
    r"^\s*\$\(\s*cat\s+<<-?\s*[\"']?([A-Za-z_][A-Za-z0-9_]*)[\"']?\s*\n"
    r"(.*?)\n\s*\1\s*\n?\s*\)\s*$",
    subject,
    re.DOTALL,
)
if heredoc_match:
    for line in heredoc_match.group(2).splitlines():
        stripped = line.strip()
        if stripped:
            subject = stripped
            break

# Take the first line — bodies passed via real shell heredocs reach us as
# multi-line text but only the first line is the Conventional Commits subject.
subject = subject.splitlines()[0].strip() if subject else subject
if not subject:
    sys.exit(0)

pattern = (
    r"^(feat|fix|docs|chore|refactor|test|build|ci|perf|style|revert|deps)"
    r"(\([^)]+\))?!?:\s.+$"
)
if re.match(pattern, subject):
    sys.exit(0)

sys.stderr.write(
    "[conventional-commit-validator] Subject does not match Conventional Commits.\n"
    f"  subject:  {subject}\n"
    "  expected: <type>[(scope)][!]: <description>\n"
    "  types:    feat fix docs chore refactor test build ci perf style revert deps\n"
)
sys.exit(2)
PY
