#!/usr/bin/env bash
# Prints release notes for a tag, grouped by Conventional Commit type.
# Usage: packaging/release-notes.sh <tag>
# Lists the commits since the previous v* tag (or all of them for the first release).
set -euo pipefail

tag="${1:?usage: release-notes.sh <tag>}"
repo="${GITHUB_REPOSITORY:-bjornharrtell/pgroove}"
prev="$(git describe --tags --abbrev=0 --match 'v*' "${tag}^" 2>/dev/null || true)"
range="${prev:+$prev..}$tag"

declare -A sections=(
  [feat]="Features" [fix]="Fixes" [perf]="Performance" [refactor]="Refactoring"
  [docs]="Documentation" [build]="Build and dependencies" [ci]="Build and dependencies"
  [test]="Tests" [style]="Other" [chore]="Other" [other]="Other"
)
declare -A body
order=("Features" "Fixes" "Performance" "Refactoring" "Documentation" "Build and dependencies" "Tests" "Other")
re='^([a-z]+)(\(([^)]+)\))?(!)?: (.+)$'

while IFS= read -r subject; do
  [ -n "$subject" ] || continue
  type=other scope="" breaking="" text="$subject"
  if [[ "$subject" =~ $re ]]; then
    type="${BASH_REMATCH[1]}"
    scope="${BASH_REMATCH[3]}"
    breaking="${BASH_REMATCH[4]}"
    text="${BASH_REMATCH[5]}"
  fi
  # Release bumps are noise in the notes.
  [[ "$type: $text" =~ ^chore:\ release ]] && continue
  section="${sections[$type]:-Other}"
  line="- "
  [ -n "$breaking" ] && line+="**Breaking:** "
  [ -n "$scope" ] && line+="**${scope}:** "
  line+="${text}"
  body[$section]+="${line}"$'\n'
done < <(git log --no-merges --reverse --pretty=format:'%s' "$range")

echo "## What's changed"
echo
found=0
for section in "${order[@]}"; do
  [ -n "${body[$section]:-}" ] || continue
  found=1
  echo "### ${section}"
  echo
  printf '%s\n' "${body[$section]}"
done
[ "$found" = 1 ] || { echo "No notable changes."; echo; }
if [ -n "$prev" ]; then
  echo "**Full changelog:** https://github.com/${repo}/compare/${prev}...${tag}"
fi
