#!/usr/bin/env bash
set -euo pipefail

# Transfer only the compiled archive output. Ordinary Nix runtime inputs
# come from binary caches; never rebuild an absent archive on a test worker.
mode="${1:?expected export, validate, cache, import or verify}"
expected=""
if [[ "$mode" == validate ]]; then
  directory="${2:?expected transfer directory}"
  partition="${3:?expected shard number}"
else
  expected="${2:?expected archive store path}"
  directory="${3:?expected transfer directory}"
  partition="${4:?expected shard number}"
fi
if [[ ! "$partition" =~ ^[1-4]$ ]]; then
  echo "invalid test shard: $partition" >&2
  exit 1
fi
payload="archive-${partition}.nar"
store_path_pattern='^/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[A-Za-z0-9+._?=-]+$'
content_files=(archive.tar.zst whole.filter shared.filter whole-inventory.json.gz shared-inventory.json.gz runtime-references)
metadata_files=(archive-path archive-references archive-content-checksums)
no_build=(--max-jobs 0 --builders '' --option fallback false)

fail() {
  echo "$*" >&2
  exit 1
}

regular_file() {
  [[ -f "$1" && ! -L "$1" ]] || fail "missing or non-regular archive transfer file: $1"
}

manifest_shape() {
  local file="$1"
  shift
  local names=("$@") lines=() index
  regular_file "$file"
  mapfile -t lines < "$file"
  [[ "${#lines[@]}" -eq "${#names[@]}" ]] || fail "unexpected checksum manifest entries: $file"
  for index in "${!names[@]}"; do
    [[ "${lines[$index]:0:64}" =~ ^[0-9a-f]{64}$ && "${lines[$index]:64}" == "  ${names[$index]}" ]] ||
      fail "invalid checksum manifest filename or digest: $file"
  done
  # Require canonical line endings too; never let a manifest choose file paths.
  printf '%s\n' "${lines[@]}" | cmp -s "$file" - || fail "non-canonical checksum manifest: $file"
}

validate_metadata() {
  local file declared_path
  for file in "${metadata_files[@]}" archive-checksums; do
    regular_file "$directory/$file"
  done
  manifest_shape "$directory/archive-content-checksums" "${content_files[@]}"
  manifest_shape "$directory/archive-checksums" "${metadata_files[@]}" "$payload"
  # The last canonical entry binds the raw fallback payload. Its bytes are not
  # needed to authenticate the small metadata before attempting substitution.
  if ! { (cd "$directory" && sha256sum "${metadata_files[@]}"); tail -n 1 "$directory/archive-checksums"; } |
      cmp -s "$directory/archive-checksums" -; then
    fail "archive metadata checksum mismatch; no Nix operation attempted"
  fi
  declared_path="$(cat "$directory/archive-path")"
  [[ "$declared_path" =~ $store_path_pattern ]] || fail "invalid producer archive store path"
  printf '%s\n' "$declared_path" | cmp -s "$directory/archive-path" - || fail "non-canonical producer archive store path"
  if [[ -n "$expected" && "$declared_path" != "$expected" ]]; then
    fail "archive transfer does not match the expected store path: $expected"
  fi
  mapfile -t references < "$directory/archive-references"
  for reference in "${references[@]}"; do
    [[ "$reference" =~ $store_path_pattern ]] || fail "invalid archive reference: $reference"
  done
}

verify_content() {
  local file actual_references started=$SECONDS
  for file in "${content_files[@]}"; do
    regular_file "$expected/$file"
  done
  [[ -s "$expected/archive.tar.zst" ]] || fail "empty restored nextest archive"
  if ! (cd "$expected" && sha256sum "${content_files[@]}") | cmp -s "$directory/archive-content-checksums" -; then
    fail "restored archive content checksum mismatch; refusing test execution"
  fi
  actual_references="$(nix-store --query --references "$expected")"
  # An empty reference list is valid; command substitution strips its newline.
  if [[ -n "$actual_references" ]]; then
    printf '%s\n' "$actual_references" | cmp -s "$directory/archive-references" - || fail "restored archive references do not match producer metadata"
  else
    [[ ! -s "$directory/archive-references" ]] || fail "restored archive references do not match producer metadata"
  fi
  echo "WADDLE_CI_METRIC phase=archive_content_verify route=$mode elapsed_seconds=$((SECONDS - started))" >&2
}

if [[ "$mode" != validate && ! "$expected" =~ $store_path_pattern ]]; then
  fail "invalid expected archive store path: $expected"
fi

case "$mode" in
  export)
    export_started=$SECONDS
    regular_file "$expected/archive-content-checksums"
    manifest_shape "$expected/archive-content-checksums" "${content_files[@]}"
    [[ -s "$expected/archive.tar.zst" ]] || fail "empty producer nextest archive"
    mkdir -p "$directory"
    printf '%s\n' "$expected" > "$directory/archive-path"
    nix-store --query --references "$expected" > "$directory/archive-references"
    install -m 0644 "$expected/archive-content-checksums" "$directory/archive-content-checksums"
    # Hash the export stream while writing it, avoiding a second 2 GB NAR scan.
    # The fixed content hashes were already produced by the archive derivation.
    payload_checksum="$(nix-store --export "$expected" | tee "$directory/$payload" | sha256sum)"
    {
      (cd "$directory" && sha256sum "${metadata_files[@]}")
      printf '%s  %s\n' "${payload_checksum%% *}" "$payload"
    } > "$directory/archive-checksums"
    wc -c "$expected/archive.tar.zst" "$directory/$payload"
    echo "WADDLE_CI_METRIC phase=archive_export elapsed_seconds=$((SECONDS - export_started))" >&2
    ;;
  validate)
    # This preflight can run before independently evaluating the current output.
    # Cache/import/verify repeat validation and bind that evaluated path below.
    validate_metadata
    ;;
  cache)
    validate_metadata
    # Keep raw diagnostics for a hard failure. Only this exact missing-output
    # response from the pinned Determinate Nix client authorizes raw fallback.
    restore_started=$SECONDS
    if restored="$(nix-store --realise "$expected" "${no_build[@]}" --log-format raw 2> "$directory/cache-restore.log")"; then
      echo "WADDLE_CI_METRIC phase=archive_cache_restore elapsed_seconds=$((SECONDS - restore_started)) success=true" >&2
      [[ "$restored" == "$expected" ]] || fail "cache restore returned an unexpected store path"
      verify_content
      echo "Verified binary-cache archive: $expected"
    else
      status=$?
      echo "WADDLE_CI_METRIC phase=archive_cache_restore elapsed_seconds=$((SECONDS - restore_started)) success=false" >&2
      cat "$directory/cache-restore.log" >&2
      if [[ "$status" -eq 1 && -z "$restored" ]] &&
          printf "don't know how to build these paths:\n  %s\nerror: path '%s' is required, but there is no substituter that can build it\n" "$expected" "$expected" |
          cmp -s "$directory/cache-restore.log" -; then
        echo "Archive absent from binary caches; raw artifact fallback is required" >&2
        exit 2
      fi
      fail "archive cache restore failed; refusing to hide a cache or integrity error with raw fallback"
    fi
    ;;
  import)
    validate_metadata
    regular_file "$directory/$payload"
    [[ -s "$directory/$payload" ]] || fail "empty archive transfer payload: $directory/$payload"
    if ! (cd "$directory" && sha256sum "${metadata_files[@]}" "$payload") | cmp -s "$directory/archive-checksums" -; then
      fail "archive transfer checksum mismatch; no Nix import attempted"
    fi
    for reference in "${references[@]}"; do
      if [[ "$reference" != "$expected" ]]; then
        nix-store --realise "${no_build[@]}" "$reference" >/dev/null
      fi
    done
    imported="$(nix-store --import < "$directory/$payload")"
    [[ "$imported" == "$expected" ]] || fail "Nix import returned an unexpected store path"
    verify_content
    ;;
  verify)
    validate_metadata
    # Never treat a workflow/environment 'hit' value as proof of valid bytes.
    nix-store --check-validity "$expected"
    verify_content
    ;;
  *) fail "unknown transfer mode: $mode" ;;
esac
