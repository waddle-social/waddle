package contributors

import "github.com/cuenv/cuenv/schema"

// cuenv 0.55 workspace sync silently skips failed CUE instances and rewrites
// runtime locks from that partial discovery. Setup only materializes pinned
// VCS dependencies; explicit per-directory CI and lock checks validate source.
// Keep the release installer aligned with upstream when upgrading cuenv.
#CuenvRelease: schema.#Contributor & {
	id: "cuenv"
	when: cuenvSource: ["release"]
	tasks: [{
		id:       "cuenv.setup"
		label:    "Setup cuenv (release)"
		priority: 10
		env: GITHUB_TOKEN: "${{ secrets.GITHUB_TOKEN }}"
		script: """
			arch="$(uname -m)"
			case "$arch" in
			  x86_64|amd64) cuenv_asset="cuenv-linux-x64" ;;
			  aarch64|arm64) cuenv_asset="cuenv-linux-arm64" ;;
			  *) echo "Unsupported Linux architecture: $arch" >&2; exit 1 ;;
			esac
			cuenv_version="${CUENV_VERSION}"
			if [ -z "$cuenv_version" ]; then
			  cuenv_version="latest"
			fi
			if [ "$cuenv_version" = "latest" ]; then
			  cuenv_url="https://github.com/cuenv/cuenv/releases/latest/download/${cuenv_asset}"
			else
			  cuenv_url="https://github.com/cuenv/cuenv/releases/download/${cuenv_version}/${cuenv_asset}"
			fi
			curl -fsSL -o /usr/local/bin/cuenv "$cuenv_url" && chmod +x /usr/local/bin/cuenv && /usr/local/bin/cuenv sync vcs -p .
			"""
	}]
}
