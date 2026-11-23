package contributors

import "github.com/cuenv/cuenv/schema"

// CLI and release digests are independent of the CUE schema dependency.
#CuenvVersion: "0.77.0"

// Bootstrap only materializes pinned VCS dependencies. Explicit per-directory
// CI and lock checks validate source; do not use partial workspace discovery.
#CuenvRelease: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["release"]

	tasks: [{
		id:       "cuenv.setup"
		label:    "Setup cuenv (release)"
		priority: 10
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		script: """
			set -euo pipefail
			cuenv_version="${CUENV_VERSION}"
			if [ "$cuenv_version" != "\(#CuenvVersion)" ]; then
			  echo "Unsupported cuenv version: $cuenv_version; reviewed digests are required" >&2
			  exit 1
			fi
			cuenv_platform="$(uname -s)-$(uname -m)"
			case "$cuenv_platform" in
			  Darwin-arm64|Darwin-aarch64)
			    cuenv_asset="cuenv-darwin-arm64"
			    cuenv_sha256="bdb3413abdea0c5617ed5a9ec3c10c5813dd6a3b8233305e81c53d76d5236647"
			    ;;
			  Linux-aarch64|Linux-arm64)
			    cuenv_asset="cuenv-linux-arm64"
			    cuenv_sha256="fde68bff3e2adc383d25bc64530b0c72b53fab32181da97af99dbed52e7a6814"
			    ;;
			  Linux-x86_64|Linux-amd64)
			    cuenv_asset="cuenv-linux-x64"
			    cuenv_sha256="d7a35a14778316c65ff725bd3c1bd776ea1610b09f02cc20badc588110899085"
			    ;;
			  *) echo "Unsupported cuenv platform: $cuenv_platform" >&2; exit 1 ;;
			esac
			if [[ ! "$cuenv_sha256" =~ ^[0-9a-f]{64}$ ]]; then
			  echo "Missing or invalid cuenv SHA-256" >&2
			  exit 1
			fi
			test -d "${RUNNER_TEMP:?RUNNER_TEMP is required}"
			: "${GITHUB_PATH:?GITHUB_PATH is required}"
			cuenv_download="$(mktemp "$RUNNER_TEMP/cuenv-download.XXXXXX")"
			trap 'rm -f "$cuenv_download"' EXIT
			cuenv_url="https://github.com/cuenv/cuenv/releases/download/${cuenv_version}/${cuenv_asset}"
			curl --proto '=https' --proto-redir '=https' -fsSL -o "$cuenv_download" "$cuenv_url"
			case "$cuenv_platform" in
			  Darwin-*) printf '%s  %s\\n' "$cuenv_sha256" "$cuenv_download" | shasum -a 256 -c - ;;
			  Linux-*) printf '%s  %s\\n' "$cuenv_sha256" "$cuenv_download" | sha256sum -c - ;;
			  *) echo "Unsupported cuenv platform: $cuenv_platform" >&2; exit 1 ;;
			esac
			chmod 0755 "$cuenv_download"
			if [ "$("$cuenv_download" --version)" != "cuenv \(#CuenvVersion)" ]; then
			  echo "Downloaded cuenv version differs from the pinned release" >&2
			  exit 1
			fi
			cuenv_bin="$(mktemp -d "$RUNNER_TEMP/waddle-cuenv.XXXXXX")"
			install -m 0755 "$cuenv_download" "$cuenv_bin/cuenv"
			printf '%s\\n' "$cuenv_bin" >> "$GITHUB_PATH"
			"$cuenv_bin/cuenv" sync vcs -p .
			"""
	}]
}
