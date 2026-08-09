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
# Usage: changelog-notes.sh <version> [changelog-path]
set -euo pipefail

version="${1:?usage: changelog-notes.sh <version> [changelog-path]}"
changelog="${2:-CHANGELOG.md}"

[ -f "$changelog" ] || exit 0

# Print everything after `## [<version>]` up to the next `## [` heading. The
# `index(...) == 1` anchors the match at the start of the line so `## [1.2.3]`
# cannot be matched by a prefix of another version.
awk -v ver="$version" '
  /^## \[/ { if (c) exit; if (index($0, "## [" ver "]") == 1) c=1; next }
  c { print }
' "$changelog" | sed '/./,$!d'
