#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
system="${1:-$(nix eval --raw --impure --expr builtins.currentSystem)}"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/waddle-nix-cache-keys.XXXXXX")"

trap 'rm -rf "${tmp}"' EXIT

export XDG_CACHE_HOME="${tmp}/xdg-cache"
mkdir -p "${XDG_CACHE_HOME}"

base="${tmp}/base"
work="${tmp}/work"

copy_base() {
  mkdir -p "${base}"
  rsync -a --delete \
    --exclude .git \
    --exclude .jj \
    --exclude result \
    --exclude target \
    --exclude node_modules \
    "${root}/" "${base}/"
}

reset_work() {
  mkdir -p "${work}"
  rsync -a --delete "${base}/" "${work}/"
}

drv_path() {
  nix eval --raw --accept-flake-config "${work}#${1}.drvPath"
}

pkg_drv() {
  drv_path "packages.${system}.${1}"
}

check_drv() {
  drv_path "checks.${system}.${1}"
}

assert_same() {
  local label="$1"
  local before="$2"
  local after="$3"

  if [[ "${before}" != "${after}" ]]; then
    echo "cache key changed unexpectedly for ${label}" >&2
    echo "before: ${before}" >&2
    echo "after:  ${after}" >&2
    exit 1
  fi
}

assert_changed() {
  local label="$1"
  local before="$2"
  local after="$3"

  if [[ "${before}" == "${after}" ]]; then
    echo "cache key did not change for ${label}" >&2
    echo "path: ${before}" >&2
    exit 1
  fi
}

copy_base
reset_work

base_server="$(pkg_drv waddle-server)"
base_server_deps="$(pkg_drv waddle-server-deps)"
base_xmpp_tests="$(check_drv waddle-server-xmpp-xep-integration)"

reset_work
printf 'cache contract touch\n' > "${work}/chat/.nix-cache-contract"
assert_same "non-server edit: waddle-server" "${base_server}" "$(pkg_drv waddle-server)"
assert_same "non-server edit: waddle-server-deps" "${base_server_deps}" "$(pkg_drv waddle-server-deps)"

reset_work
printf '\n// cache contract touch\n' >> "${work}/server/crates/waddle-xmpp-core/src/lib.rs"
assert_changed "rust source edit: waddle-server" "${base_server}" "$(pkg_drv waddle-server)"
assert_same "rust source edit: waddle-server-deps" "${base_server_deps}" "$(pkg_drv waddle-server-deps)"

reset_work
printf '\n// cache contract touch\n' >> "${work}/server/crates/waddle-xmpp/tests/xep0080_pep_geoloc.rs"
assert_same "test-only edit: waddle-server" "${base_server}" "$(pkg_drv waddle-server)"
assert_same "test-only edit: waddle-server-deps" "${base_server_deps}" "$(pkg_drv waddle-server-deps)"
assert_changed "test-only edit: xep integration check" "${base_xmpp_tests}" "$(check_drv waddle-server-xmpp-xep-integration)"

reset_work
printf '\n[dev-dependencies.cache-contract-touch]\nversion = "0.0.0"\n' >> "${work}/server/crates/waddle-xmpp-core/Cargo.toml"
assert_changed "manifest edit: waddle-server-deps" "${base_server_deps}" "$(pkg_drv waddle-server-deps)"

echo "Nix cache-key contract holds for ${system}."
