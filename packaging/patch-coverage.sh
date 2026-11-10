#!/bin/sh
# Prints line coverage for the lines changed since a base revision, for the 80% patch-coverage
# threshold in AGENTS.md (a workspace total cannot show whether a patch itself is tested).
# Runs locally, no CI needed.
#
# Usage: packaging/patch-coverage.sh [base-revision]
#   The base revision defaults to origin/main; the merge base with HEAD is what is diffed, so a
#   tag, a commit or main all work.
# Environment: PATCH_COVERAGE_MIN sets the threshold in percent (default 80).
#
# Needs git, awk and cargo-llvm-cov with the LLVM tools:
#   rustup component add llvm-tools-preview
#   cargo install cargo-llvm-cov        (or the prebuilt action the CI coverage job uses)
#
# Like the CI coverage job it runs the whole workspace without PGROOVE_TEST_PG, so the tests that
# need a database skip themselves and the numbers do not depend on a local server. Lines inside
# #[cfg(test)] modules count as ordinary lines; they run, so they are almost always covered.
set -eu

minimum="${PATCH_COVERAGE_MIN:-80}"
base="${1:-origin/main}"
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

if ! base_sha=$(git merge-base "$base" HEAD 2>/dev/null); then
  echo "patch-coverage: no such base revision '$base' (fetch it first, or pass one)" >&2
  exit 2
fi
echo "patch-coverage: diffing against $base ($(git rev-parse --short "$base_sha"))"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# New-side line numbers of every changed line of Rust source, one "path<TAB>line" per row.
git diff --unified=0 --no-color "$base_sha" HEAD -- crates |
  awk -v OFS='\t' '
    /^\+\+\+ / { path = ($2 == "/dev/null") ? "" : substr($2, 3); next }
    /^@@ / && path ~ /\.rs$/ {
      split($3, hunk, ",")                    # "@@ -a,b +c,d @@": $3 is "+c,d"
      start = substr(hunk[1], 2) + 0
      count = (2 in hunk) ? hunk[2] + 0 : 1
      for (i = 0; i < count; i++) print path, start + i
    }
  ' > "$work/changed"

if [ ! -s "$work/changed" ]; then
  echo "patch-coverage: no Rust lines changed since $base"
  exit 0
fi

# Same measurement as the CI coverage job: the whole workspace, no database environment.
cargo llvm-cov --workspace --no-fail-fast --lcov --output-path "$work/lcov.info"

awk -v root="$root" -v minimum="$minimum" -v OFS='\t' '
  NR == FNR { changed[$1 OFS $2] = 1; next }
  /^SF:/ {
    path = substr($0, 4)
    if (substr(path, 1, length(root) + 1) == root "/") path = substr(path, length(root) + 2)
    next
  }
  /^DA:/ {
    split(substr($0, 4), hit, ",")
    key = path OFS hit[1]
    if (key in changed) {
      total++
      if (hit[2] + 0 > 0) {
        covered++
      } else if (missed++ < 20) {
        missed_list = missed_list sprintf("  %s:%s\n", path, hit[1])
      }
    }
    next
  }
  END {
    # Changed lines that are blank, comments or not instrumented never appear in the report.
    percent = total ? 100 * covered / total : 100
    printf "patch coverage: %.1f%% (%d of %d changed executable lines covered), threshold %d%%\n", \
      percent, covered, total, minimum
    if (total == 0) {
      printf "patch-coverage: no changed line is in the coverage report; expected for a patch of"
      printf " comments or blank lines, otherwise the report paths do not match this checkout\n"
      exit 0
    }
    if (missed_list != "") printf "uncovered changed lines:\n%s", missed_list
    if (percent + 0 < minimum + 0) {
      printf "patch-coverage: below the %d%% threshold\n", minimum
      exit 1
    }
  }
' "$work/changed" "$work/lcov.info"
