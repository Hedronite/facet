#!/usr/bin/env bash
# Fail on any GitHub Actions `uses:` that is not pinned to a full 40-hex
# commit SHA with a trailing `# <tag>` comment. Local `./` actions are
# allowed. Comment-only lines are ignored, and a SHA that appears only in a
# trailing comment does not count as a pin.
#
#   scripts/check-action-pins.sh [FILE|DIR ...]   default: .github/workflows
#   scripts/check-action-pins.sh --probes [DIR]   default: scripts/testdata/action-pins
#
# --probes runs the checker on each probe file: every bad-*.yml must fail
# and every good-*.yml must pass.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)

uses_re='^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*(.*)$'
pinned_re='^[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}$'
tag_re='[^[:space:]]'

# check_file FILE: print each bad `uses:` line; return 1 if any.
check_file() {
  local file=$1 n=0 bad=0 line rest ref comment
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    [[ $line =~ ^[[:space:]]*# ]] && continue
    [[ $line =~ $uses_re ]] || continue
    rest=${BASH_REMATCH[2]}
    ref=${rest%%#*}
    comment=
    if [[ $rest == *"#"* ]]; then
      comment=${rest#*#}
    fi
    # Trim whitespace and one layer of quotes.
    ref=$(printf '%s' "$ref" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
    ref=${ref#\"}
    ref=${ref%\"}
    ref=${ref#\'}
    ref=${ref%\'}
    case $ref in
      ./*) continue ;;
    esac
    if [[ $ref =~ $pinned_re ]] && [[ $comment =~ $tag_re ]]; then
      continue
    fi
    echo "$file:$n: not pinned to a 40-hex SHA with a # <tag> comment: $ref" >&2
    bad=1
  done <"$file"
  return "$bad"
}

# workflow_files PATH...: the .yml / .yaml files under each PATH.
workflow_files() {
  local p
  for p in "$@"; do
    if [ -d "$p" ]; then
      find "$p" -type f \( -name '*.yml' -o -name '*.yaml' \) | LC_ALL=C sort
    elif [ -f "$p" ]; then
      printf '%s\n' "$p"
    else
      echo "check-action-pins: no such file or directory: $p" >&2
      return 1
    fi
  done
}

run_probes() {
  local dir=$1 f name status=0 bad=0 good=0
  for f in $(workflow_files "$dir"); do
    name=$(basename "$f")
    case $name in
      bad-*)
        bad=$((bad + 1))
        if check_file "$f" 2>/dev/null; then
          echo "probe $name: passed, but must fail" >&2
          status=1
        else
          echo "probe $name: fails as expected"
        fi
        ;;
      good-*)
        good=$((good + 1))
        if check_file "$f"; then
          echo "probe $name: passes as expected"
        else
          echo "probe $name: failed, but must pass" >&2
          status=1
        fi
        ;;
      *)
        echo "probe $name: name must start with bad- or good-" >&2
        status=1
        ;;
    esac
  done
  if [ "$bad" -eq 0 ] || [ "$good" -eq 0 ]; then
    echo "check-action-pins: $dir needs at least one bad- and one good- probe" >&2
    status=1
  fi
  return "$status"
}

if [ "${1:-}" = "--probes" ]; then
  run_probes "${2:-$root/scripts/testdata/action-pins}"
  exit
fi

[ "$#" -gt 0 ] || set -- "$root/.github/workflows"
status=0
files=$(workflow_files "$@")
[ -n "$files" ] || {
  echo "check-action-pins: no workflow files found" >&2
  exit 1
}
for f in $files; do
  check_file "$f" || status=1
done
if [ "$status" -eq 0 ]; then
  echo "check-action-pins: every uses: is pinned to a 40-hex SHA"
fi
exit "$status"
