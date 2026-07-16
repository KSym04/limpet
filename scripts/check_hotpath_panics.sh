#!/usr/bin/env bash
#
# Ratchet guard against new panic sites on the MCP hot path.
#
# A panic inside tools::dispatch kills the agent's live MCP session, which is
# the worst first impression limpet can make. There are already many
# unwrap/expect calls in these modules; this script does NOT demand they all
# vanish at once. It pins the current count per file as a ceiling and fails CI
# if any file exceeds it. As you convert sites to `?`, lower the number in
# scripts/hotpath_panic_baseline.txt. It can only go down.
#
# Usage:  scripts/check_hotpath_panics.sh
# Exit:   0 if every file is at or below its baseline, 1 otherwise.

set -euo pipefail

cd "$(dirname "$0")/.."

baseline="scripts/hotpath_panic_baseline.txt"
pattern='\.unwrap\(\)|\.expect\(|panic!|unreachable!|unimplemented!|todo!'
status=0

# Count panic sites on the HOT PATH only. A panic in a `#[cfg(test)]` module
# cannot kill a live MCP session, so it is excluded: everything from the first
# `#[cfg(test)]` line to end-of-file is dropped before counting. Files with no
# test module are counted whole.
count_hotpath() {
  awk 'BEGIN { p = 1 } /#\[cfg\(test\)\]/ { p = 0 } p' "$1" | grep -cE "$pattern" || true
}

while IFS=' ' read -r allowed file; do
  # Skip blank lines and comments.
  [[ -z "${allowed:-}" || "${allowed:0:1}" == "#" ]] && continue
  # A renamed or deleted file would count as 0 and guard nothing; a stale
  # baseline list is a hard failure, matching the can-only-go-down rule.
  if [[ ! -f "$file" ]]; then
    echo "FAIL $file: listed in the baseline but does not exist (renamed? update the baseline)"
    status=1
    continue
  fi
  actual=$(count_hotpath "$file")
  if (( actual > allowed )); then
    echo "FAIL $file: $actual panic sites, baseline is $allowed"
    status=1
  elif (( actual < allowed )); then
    echo "note $file: $actual < baseline $allowed. Lower the baseline to lock in the gain."
  fi
done < "$baseline"

if (( status == 0 )); then
  echo "ok: no hot-path panic regressions"
fi
exit "$status"
