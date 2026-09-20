#!/usr/bin/env bash
set -euo pipefail

# The metadata and shard archives travel through Actions artifacts. Keep
# these outputs out of the binary cache while preserving dependency uploads.
installable="${1:-../#waddle-server-test-archive}"
expected="$(nix eval --raw "$installable.outPath")"
all_outputs="$(nix eval --raw "$installable" --apply \
  'drv: builtins.concatStringsSep "\n" (map (name: drv.${name}.outPath) drv.outputs)')"
mapfile -t excluded_outputs <<< "$all_outputs"
for output in "$expected" "${excluded_outputs[@]}"; do
  if [[ ! "$output" =~ ^/nix/store/[a-z0-9]{32}-[^/[:space:]]+$ ]]; then
    echo "invalid expected archive store path: $output" >&2
    exit 1
  fi
done

# Read only this setting; the complete Nix configuration can contain tokens.
original_hook="$(nix config show post-build-hook)"
if [[ -z "$original_hook" ]]; then
  nix build --print-build-logs --no-link "$installable^*"
  printf '%s\n' "$expected"
  exit 0
fi
if [[ ! -x "$original_hook" ]]; then
  echo "configured Nix post-build hook is not executable" >&2
  exit 1
fi

hook_directory="$(mktemp -d)"
trap 'rm -rf "$hook_directory"' EXIT
hook="$hook_directory/post-build-hook"
{
  printf '#!%s\n' "$(command -v bash)"
  printf 'excluded=('
  printf ' %q' "${excluded_outputs[@]}"
  printf ' )\noriginal_hook=%q\n' "$original_hook"
  cat <<'HOOK'
set -euo pipefail
# Nix supplies OUT_PATHS as space-separated store paths. Embed the
# configuration values above because the daemon does not inherit our env.
read -r -a built_paths <<< "${OUT_PATHS:-}"
retained=()
for output in "${built_paths[@]}"; do
  skip=false
  for archive_output in "${excluded[@]}"; do
    if [[ "$output" == "$archive_output" ]]; then
      echo "Skipping duplicate binary-cache upload of nextest archive output: $output" >&2
      skip=true
      break
    fi
  done
  if [[ "$skip" == false ]]; then
    retained+=("$output")
  fi
done
if (( ${#retained[@]} )); then
  export OUT_PATHS="${retained[*]}"
  exec "$original_hook"
fi
HOOK
} > "$hook"
chmod 700 "$hook"
# Request every output even when a cache already contains metadata alone.
nix build --print-build-logs --no-link \
  --option post-build-hook "$hook" "$installable^*"
printf '%s\n' "$expected"
