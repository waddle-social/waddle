#!/usr/bin/env bash
set -euo pipefail

# Transfer only the compiled archive output. Ordinary Nix runtime inputs
# come from binary caches; never transfer a complete compiler/dependency
# closure or rebuild an absent archive on a test worker.
mode="${1:?expected export or import}"
expected="${2:?expected archive store path}"
directory="${3:?expected transfer directory}"

if [[ ! "$expected" =~ ^/nix/store/[a-z0-9]{32}-[^/]+$ ]]; then
  echo "invalid expected archive store path: $expected" >&2
  exit 1
fi

case "$mode" in
  export)
    test -s "$expected/archive.tar.zst"
    mkdir -p "$directory"
    printf '%s\n' "$expected" > "$directory/archive-path"
    nix-store --query --references "$expected" > "$directory/archive-references"
    nix-store --export "$expected" > "$directory/archive.nar"
    wc -c "$expected/archive.tar.zst" "$directory/archive.nar"
    ;;
  import)
    test "$(cat "$directory/archive-path")" = "$expected"
    while IFS= read -r reference; do
      if [[ ! "$reference" =~ ^/nix/store/[a-z0-9]{32}-[^/]+$ ]]; then
        echo "invalid archive reference: $reference" >&2
        exit 1
      fi
      if [[ "$reference" != "$expected" ]]; then
        nix-store --realise --option max-jobs 0 "$reference" >/dev/null
      fi
    done < "$directory/archive-references"
    imported="$(nix-store --import < "$directory/archive.nar")"
    test "$imported" = "$expected"
    test -s "$expected/archive.tar.zst"
    ;;
  *)
    echo "unknown transfer mode: $mode" >&2
    exit 1
    ;;
esac
