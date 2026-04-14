#!/usr/bin/env bash
set -euo pipefail

# Generate self-signed localhost TLS certificates for local development.
# Outputs to server/certs/ and server/local/certs/.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SERVER_DIR="$(dirname "$SCRIPT_DIR")"

FORCE=false
if [[ "${1:-}" == "--force" ]]; then
  FORCE=true
fi

generate_cert() {
  local dir="$1"
  local key="$dir/server.key"
  local crt="$dir/server.crt"

  if [[ -f "$key" && -f "$crt" && "$FORCE" == "false" ]]; then
    echo "Certs already exist in $dir (use --force to regenerate)"
    return
  fi

  mkdir -p "$dir"
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$key" \
    -out "$crt" \
    -days 3650 \
    -subj "/CN=localhost" \
    -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
    2>/dev/null

  echo "Generated certs in $dir"
}

generate_cert "$SERVER_DIR/certs"
generate_cert "$SERVER_DIR/local/certs"
