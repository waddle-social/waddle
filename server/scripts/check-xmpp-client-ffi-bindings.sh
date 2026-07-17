#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVER_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$SERVER_ROOT/.." && pwd)"
CARGO_TARGET_ROOT="${CARGO_TARGET_DIR:-$SERVER_ROOT/target}"
if [[ "$CARGO_TARGET_ROOT" != /* ]]; then
  CARGO_TARGET_ROOT="$REPO_ROOT/$CARGO_TARGET_ROOT"
fi
export CARGO_TARGET_DIR="$CARGO_TARGET_ROOT"
TMP_ROOT="${TMPDIR:-/tmp}"
if [[ "$TMP_ROOT" != /* ]]; then
  TMP_ROOT="$REPO_ROOT/$TMP_ROOT"
fi
mkdir -p "$TMP_ROOT"
export TMPDIR="$TMP_ROOT"
GENERATED_DIR="$REPO_ROOT/apps/apple/Waddle/RustClient/Generated"

case "$(uname -s)" in
  Darwin) LIB_EXT="dylib" ;;
  Linux) LIB_EXT="so" ;;
  *) echo "unsupported host for UniFFI binding drift check: $(uname -s)" >&2; exit 1 ;;
esac

TMP_DIR="$(mktemp -d "$TMPDIR/waddle-apple-ffi-bindings.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

cargo build \
  --manifest-path "$SERVER_ROOT/Cargo.toml" \
  --locked \
  -p waddle-xmpp-client-ffi

(cd "$SERVER_ROOT" && cargo run -p waddle-xmpp-client-ffi \
  --locked \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "$CARGO_TARGET_ROOT/debug/libwaddle_xmpp_client_ffi.${LIB_EXT}" \
  --language swift \
  --out-dir "$TMP_DIR")

perl -pi -e 's/[ \t]+$//' \
  "$TMP_DIR/waddle_xmpp_client.swift" \
  "$TMP_DIR/waddle_xmpp_clientFFI.h"

for file in waddle_xmpp_client.swift waddle_xmpp_clientFFI.h waddle_xmpp_clientFFI.modulemap; do
  if ! diff -u "$GENERATED_DIR/$file" "$TMP_DIR/$file"; then
    echo "Apple UniFFI binding drift detected in $file." >&2
    echo "Run scripts/generate-xmpp-client-ffi-bindings.sh and commit the regenerated binding files." >&2
    exit 1
  fi
done
