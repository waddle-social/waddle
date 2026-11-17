#!/usr/bin/env bash
# Start a runner shape benchmark (ci/runner-benchmark) for the current commit.
#
# Pushes HEAD plus one throwaway commit to ci-benchmark/<name>. The extra
# commit appends a comment to the server crate, so every variant compiles a
# source tree no cache has seen, as a pull request that edits the server does.
# Without it a second pass would substitute the first pass's archives and
# report no compile time. The working tree and current branch are not touched.
set -euo pipefail

name="${1:-runner-shapes}"
variants="${2:-}"
if [[ "$#" -gt 2 || ! "$name" =~ ^[a-z0-9][a-z0-9-]*$ || ! "$variants" =~ ^([0-9a-z-]+(,[0-9a-z-]+)*)?$ ]]; then
  echo "Usage: $0 [branch-name [variant,variant...]]   (pushes to ci-benchmark/<branch-name>)" >&2
  exit 2
fi
cd "$(git rev-parse --show-toplevel)"
file=server/crates/waddle-server/src/lib.rs
head="$(git rev-parse HEAD)"
index="$(mktemp)"
trap 'rm -f "$index"' EXIT
export GIT_INDEX_FILE="$index"
git read-tree "$head"
blob="$({ git show "$head:$file"; printf '// ci-benchmark nonce %s\n' "$(date -u +%Y%m%dT%H%M%SZ)"; } | git hash-object -w --stdin)"
git update-index --cacheinfo "100644,$blob,$file"
commit="$(git commit-tree "$(git write-tree)" -p "$head" -m "chore(ci): runner benchmark nonce${variants:+ variants=$variants}")"
git push --force origin "$commit:refs/heads/ci-benchmark/$name"
echo "Benchmark started for $head as $commit on ci-benchmark/$name"
