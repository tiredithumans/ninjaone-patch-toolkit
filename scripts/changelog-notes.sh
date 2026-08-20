#!/usr/bin/env bash
# Prints the CHANGELOG.md section for one version, with leading blank lines
# trimmed. Prints nothing when there is no such section.
#
# This exists because the same awk sat inline in three places in release.yml —
# the guard that fails a tag whose notes were never rolled, and the two steps that
# feed the GitHub release body and the updater manifest's `notes`. The guard is
# only meaningful if it runs the *identical* extraction as the steps it protects,
# and three hand-kept copies is exactly the shape that drifts: a format change
# fixed in one leaves the other two matching nothing, and the failure is silent —
# a release ships with placeholder notes, which is also what the in-app update
# window shows.
#
# With --github-env, writes the notes to $GITHUB_ENV as RELEASE_NOTES instead of
# stdout, substituting a placeholder when the section is empty. That wrapper — the
# emptiness test, the placeholder text and the heredoc-delimited env write — used to
# sit inline and identical in two release.yml steps, which is the same drift shape
# that put the awk here in the first place: the release body and the updater
# manifest's `notes` must say the same thing, and nothing checked that they did.
# The bare form still prints nothing for a missing section, which is what the guard
# needs to fail a tag whose notes were never rolled.
#
# Usage: changelog-notes.sh [--github-env] <version> [changelog-path]
set -euo pipefail

github_env=no
if [ "${1:-}" = "--github-env" ]; then
  github_env=yes
  shift
fi

version="${1:?usage: changelog-notes.sh [--github-env] <version> [changelog-path]}"
changelog="${2:-CHANGELOG.md}"

emit() {
  if [ "$github_env" = no ]; then
    printf '%s\n' "$1"
    return
  fi
  notes="$1"
  if [ -z "$(printf %s "$notes" | tr -d '[:space:]')" ]; then
    notes="See the release notes on GitHub for what's new in v$version."
  fi
  {
    echo "RELEASE_NOTES<<__CHANGELOG_EOF__"
    echo "$notes"
    echo "__CHANGELOG_EOF__"
  } >> "${GITHUB_ENV:?--github-env requires GITHUB_ENV to be set}"
}

if [ ! -f "$changelog" ]; then
  emit ""
  exit 0
fi

# Print everything after `## [<version>]` up to the next `## [` heading. The
# `index(...) == 1` anchors the match at the start of the line so `## [1.2.3]`
# cannot be matched by a prefix of another version.
emit "$(awk -v ver="$version" '
  /^## \[/ { if (c) exit; if (index($0, "## [" ver "]") == 1) c=1; next }
  c { print }
' "$changelog" | sed '/./,$!d')"
