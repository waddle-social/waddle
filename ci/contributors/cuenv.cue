package contributors

import "github.com/cuenv/cuenv/schema"

// #PinnedCuenvRelease installs a cuenv release known to publish Linux assets.
#PinnedCuenvRelease: schema.#Contributor & {
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
			curl -fsSL -o /usr/local/bin/cuenv "https://github.com/cuenv/cuenv/releases/download/0.41.3/${cuenv_asset}" && chmod +x /usr/local/bin/cuenv && /usr/local/bin/cuenv sync -A
			"""
	}]
}
