#!/usr/bin/env bash
# Transfer the test member layer and its references through a run-scoped artifact.
set -euo pipefail
operation="${1:?expected export or restore}"
layer="${2:?expected evaluated Nix output path}"
cache="file://${RUNNER_TEMP:?}/rust-layer-cache"
store="${NIX_REMOTE:-daemon}"
nix_bin="$(command -v nix)"

# Bind the full reference graph to the producer's job output. The consumer
# independently evaluates the top path from the unedited checkout.
closure_digest() {
  nix path-info --offline --store "$1" --json --recursive "$layer" |
    jq -cS 'with_entries(.value |= {narHash, references: (.references | sort)})' |
    sha256sum | cut -d' ' -f1
}

case "$operation" in
  export)
    digest="$(closure_digest "$store")"
    nix copy --offline --from "$store" --to "$cache?compression=zstd" "$layer"
    [[ "$(closure_digest "$cache")" == "$digest" ]]
    echo "closure=$digest" >> "${GITHUB_OUTPUT:?}"
    du -sb "${RUNNER_TEMP}/rust-layer-cache"
    ;;
  restore)
    if nix-store --store "$store" --check-validity "$layer" >/dev/null 2>&1; then
      echo "Warm runner already has the top member layer before artifact import" >&2
      exit 1
    fi
    [[ "${EXPECTED_CLOSURE:?}" =~ ^[0-9a-f]{64}$ ]]
    [[ "$(closure_digest "$cache")" == "$EXPECTED_CLOSURE" ]]
    # The verified artifact is the only source of unsigned paths on this
    # ephemeral runner. A local test store needs no elevated privileges.
    as_owner=()
    if [[ "$store" == daemon ]]; then as_owner=(sudo -n); fi
    "${as_owner[@]}" "$nix_bin" copy --offline --no-check-sigs --from "$cache" --to "$store" "$layer"
    [[ "$(closure_digest "$store")" == "$EXPECTED_CLOSURE" ]]
    ;;
  *) echo "expected export or restore" >&2; exit 2 ;;
esac
