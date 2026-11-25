package contributors

import "github.com/cuenv/cuenv/schema"

// CLI and release digests are independent of the CUE schema dependency.
#CuenvVersion: "0.77.1"

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
			    cuenv_sha256="9672bad1d62c3390c84bb5fc209b3dabb8b5fb140dc3d4f37f106808da742097"
			    ;;
			  Linux-aarch64|Linux-arm64)
			    cuenv_asset="cuenv-linux-arm64"
			    cuenv_sha256="715d11f172013f115c8ba4c06c0ad9f5987ed5750a050e575f4a51fa541c3e3e"
			    ;;
			  Linux-x86_64|Linux-amd64)
			    cuenv_asset="cuenv-linux-x64"
			    cuenv_sha256="ebbd618ab47cec51a8cdd6ec723584317c9f0e9ab8c776a8c86ac0abbcdd7fa7"
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
