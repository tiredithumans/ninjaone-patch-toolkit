#!/usr/bin/env bash
# Fails when deny.toml's inbound license allow-list and about.toml's outbound
# accepted-list disagree.
#
# They are two hand-maintained copies of one policy: deny.toml decides what may
# enter the dependency tree, about.toml decides what THIRD-PARTY-LICENSES.md will
# render. A license added to one and not the other means either a crate passes the
# gate but breaks `just licenses`, or the notice claims terms the project never
# agreed to accept. This repo has been bitten by parallel hand-maintained lists
# more than once; this is the guard.

set -euo pipefail

cd "$(dirname "$0")/.."

extract() {
  python3 - "$1" "$2" <<'PY'
import re, sys
body = open(sys.argv[1]).read()
m = re.search(rf'{sys.argv[2]}\s*=\s*\[(.*?)\]', body, re.S)
if not m:
    sys.exit(f"could not find {sys.argv[2]} in {sys.argv[1]}")
print("\n".join(sorted(re.findall(r'"([^"]+)"', m.group(1)))))
PY
}

deny=$(extract deny.toml allow)
about=$(extract about.toml accepted)

if [ "$deny" = "$about" ]; then
  echo "license lists agree ($(echo "$deny" | wc -l | tr -d ' ') licenses)"
  exit 0
fi

echo "deny.toml [licenses] allow and about.toml accepted disagree:" >&2
diff <(echo "$deny") <(echo "$about") \
  --label "deny.toml allow" --label "about.toml accepted" -u >&2 || true
exit 1
