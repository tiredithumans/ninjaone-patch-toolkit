---
name: ship
description: Land the current working-tree changes on main — branch, conventional commits, push, PR, merge, branch cleanup. Use when the user says "ship", "ship this/it", "land this", or asks for the full commit → PR → merge flow.
argument-hint: "[optional scope hint or PR title]"
---

# Ship — commit → push → PR → merge → cleanup

Land the current changes on `main` end to end, without stopping to ask between steps. If
arguments were passed, treat them as guidance (what to include, scope hint, or PR title). Stop
and ask only if the working tree mixes clearly unrelated work and the split is ambiguous, or if
a gate fails.

## 0. Preflight

- `git status --short` and `git log origin/main..HEAD --oneline`. Nothing to commit **and**
  nothing unpushed → report "nothing to ship" and stop.
- **Gates:** Rust source changed (`src-tauri/`, `web-rs/`) → `just verify`, stop on failure.
  `.claude/hooks/` or `.claude/settings.json` changed → `.claude/hooks/test.sh`. Changes limited
  to docs, skills or `.github/` skip verify — say so in the PR test plan. Merging here does not
  wait on remote checks, so the local gates are the only gates.
- User-facing change without a `CHANGELOG.md` `[Unreleased]` line → add one before committing.

## 1. Branch

- On `main`? `git checkout -b <type>/<short-slug>` where `<type>` is the dominant
  conventional-commit type. Already on a topic branch? Stay on it.

## 2. Commit

- One concern per commit, Conventional Commits (`<type>[(scope)][!]: <description>`; scopes
  `desktop`, `web`, `api`, `auth`, `export`, `filter`, `settings`, `ci`, `docs`). Pass multi-line
  bodies with the `-m "$(cat <<'EOF' … EOF)"` heredoc form; the validator hook understands it,
  including double quotes inside the body.
- Stage explicitly (`git add <paths>`), never a blind `git add -A`.

## 3. Push + PR

- `git push -u origin <branch>`
- `gh pr create --base main --title "<conventional subject>" --body …` — body is `## Summary`
  (what + why, bulleted) and `## Test plan` (checkboxes for what was *actually* run; an unchecked
  box is better than a false one).

## 4. Merge + cleanup

- `gh pr merge <num> --merge --delete-branch` — merge commits; the command also checks out
  `main`, fast-forwards it, and deletes the local branch.
- `git fetch --prune`, confirm `git status` is clean on the updated `main`, report the PR URL and
  merge commit.

## Failure handling

- Commit blocked by the validator hook → fix the message; never `--no-verify`.
- A gate fails → stop, report the output, do not push.
- PR not mergeable (conflict, branch protection) → stop and report; never force-push or use admin
  overrides.
