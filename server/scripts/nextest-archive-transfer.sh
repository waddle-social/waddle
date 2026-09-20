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
    # Keep the large NAR as a raw artifact; a separate small artifact carries
    # this manifest and metadata. Bind all three files before publication.
    (cd "$directory" && sha256sum archive-path archive-references archive.nar) > "$directory/archive-checksums"
    wc -c "$expected/archive.tar.zst" "$directory/archive.nar"
    ;;
  import)
    # Download actions can report success after a partial transfer. Reject a
    # missing, truncated, or mixed artifact before invoking Nix at all. A
    # reference list can legitimately be empty, but the file must exist.
    for file in archive-path archive-references archive.nar archive-checksums; do
      if [[ ! -f "$directory/$file" ]]; then
        echo "missing archive transfer file: $directory/$file" >&2
        exit 1
      fi
    done
    if [[ ! -s "$directory/archive.nar" ]]; then
      echo "empty archive transfer payload: $directory/archive.nar" >&2
      exit 1
    fi
    if ! (cd "$directory" && sha256sum archive-path archive-references archive.nar) | cmp -s "$directory/archive-checksums" -; then
      echo "archive transfer checksum mismatch; no Nix import attempted" >&2
      exit 1
    fi
    if ! printf '%s\n' "$expected" | cmp -s "$directory/archive-path" -; then
      echo "archive transfer does not match the expected store path: $expected" >&2
      exit 1
    fi
    mapfile -t references < "$directory/archive-references"
    for reference in "${references[@]}"; do
      if [[ ! "$reference" =~ ^/nix/store/[a-z0-9]{32}-[^/]+$ ]]; then
        echo "invalid archive reference: $reference" >&2
        exit 1
      fi
    done
    for reference in "${references[@]}"; do
      if [[ "$reference" != "$expected" ]]; then
        nix-store --realise --option max-jobs 0 "$reference" >/dev/null
      fi
    done
    imported="$(nix-store --import < "$directory/archive.nar")"
    test "$imported" = "$expected"
    test -s "$expected/archive.tar.zst"
    ;;
  *)
    echo "unknown transfer mode: $mode" >&2
    exit 1
    ;;
esac
