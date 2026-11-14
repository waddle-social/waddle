package contributors

import "github.com/cuenv/cuenv/schema"

// CLI and release digests are independent of the CUE schema dependency.
#CuenvVersion: "0.74.0"

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
			    cuenv_sha256="3c2402f031b13c24ce75b8990decf2c498c067463699d801af047e3ae7f04dc7"
			    ;;
			  Linux-aarch64|Linux-arm64)
			    cuenv_asset="cuenv-linux-arm64"
			    cuenv_sha256="1ebf6c56590dcd980ed3dfe59c70f026cc3111cfc631171622729c068af9f769"
			    ;;
			  Linux-x86_64|Linux-amd64)
			    cuenv_asset="cuenv-linux-x64"
			    cuenv_sha256="35b1b64475d70606bad0e8c2dd0657478200d503294f1ff04db97bdf220c1f0f"
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
