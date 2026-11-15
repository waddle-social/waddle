package contributors

import "github.com/cuenv/cuenv/schema"

// #Nix mirrors cuenv's Nix contributor while pinning the GitHub Action to the
// immutable revision behind v3.22.3. Keep the script fallback aligned with the
// upstream contributor when upgrading cuenv.
#Nix: schema.#Contributor & {
	id: "nix"
	tasks: [{
		id:       "nix.install"
		label:    "Install Determinate Nix"
		priority: 2
		script:   "curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh -s -- install linux --no-confirm --init none"
		ci: github: permissions: "id-token": "write"
		provider: github: {
			uses: "DeterminateSystems/determinate-nix-action@021c8a1bd3570eb21f5c20a054812b0c4d9ca614"
			with: "extra-conf": "accept-flake-config = true"
		}
	}]
}

// #FlakeHubCache authenticates the runner against FlakeHub Cache over the
// job's OIDC token (declared below as `id-token: write`) so store paths
// built on one ref are substitutable on every other. GitHub's own Actions
// cache, which Hestia sits on, is scoped per ref and PR builds can never
// feed `main`; this is the shared layer that closes that gap. The GHA cache
// side of the action is disabled so it never competes with Hestia.
//
// The daemon uploads every path the job adds to the store, substituted ones
// included, as soon as it lands: five parallel compressors, no throttle,
// concurrent with whatever the job is building. Paths FlakeHub already
// holds are skipped (attic push session). A job whose build alone nears the
// runner's memory must therefore not be the first to see a large artifact:
// server `nixTest` (~6 GB rustc peak on an 8x16 runner) got SIGKILLed three
// times in a row while the multi-GB cargo deps streamed out, and passed
// once the step was removed (PR #1774). Such jobs depend on a light task
// that pulls their large inputs first, e.g. server `nixBuildDeps`.
#FlakeHubCache: schema.#Contributor & {
	id: "flakehub-cache"
	tasks: [{
		id:       "flakehub-cache.setup"
		label:    "Setup FlakeHub Cache"
		priority: 3
		dependsOn: ["nix.install"]
		ci: github: permissions: "id-token": "write"
		provider: github: {
			uses: "DeterminateSystems/flakehub-cache-action@e80fccc9c4e3a885ad51721c8b92ba3f839d2f6f"
			with: "use-gha-cache": "disabled"
		}
	}]
}

#HestiaVersion: "v2.0.0"

// #Hestia mirrors cuenv 0.55's Hestia contributor without enabling its
// repository-wide generated workflow. Waddle owns that workflow separately so
// its privileged Nix installer can also be pinned to an immutable revision.
//
// The action's own release download is a single fetch with no retry, and one
// transient network error fails the whole job before it starts its real work.
// `hestia.fetch` downloads the release binary with retries and checks it
// against digests pinned here (taken from the release and checked with
// `gh attestation verify --repo Mic92/hestia`), then hands the verified file
// to the action through its `binary` input, so the action makes no download
// of its own. Neither the action nor the generated step supports
// `continue-on-error`, and a cache step must not be able to hide a failure.
#Hestia: schema.#Contributor & {
	id: "hestia"
	tasks: [{
		id:       "hestia.fetch"
		label:    "Fetch Hestia"
		priority: 4
		script: """
			set -euo pipefail
			hestia_platform="$(uname -s)-$(uname -m)"
			case "$hestia_platform" in
			  Darwin-arm64|Darwin-aarch64)
			    hestia_asset="hestia-aarch64-darwin"
			    hestia_sha256="bb3da08432fb28436dd617359c3516d1c1373585311b88dc6fb50f8f6f40ffbe"
			    ;;
			  Linux-aarch64|Linux-arm64)
			    hestia_asset="hestia-aarch64-linux"
			    hestia_sha256="67c849c35a07b4562afccf4b0fb4b0f82ff6732cd8d4503c5f6d02b7e1e5637a"
			    ;;
			  Linux-x86_64|Linux-amd64)
			    hestia_asset="hestia-x86_64-linux"
			    hestia_sha256="f6c8fe84dc425e99e14f893ca708bba8254d587536e591471df1fc79beb5aed5"
			    ;;
			  *) echo "Unsupported Hestia platform: $hestia_platform" >&2; exit 1 ;;
			esac
			test -d "${RUNNER_TEMP:?RUNNER_TEMP is required}"
			hestia_dir="$RUNNER_TEMP/hestia-release"
			mkdir -p "$hestia_dir"
			hestia_binary="$hestia_dir/hestia"
			hestia_url="https://github.com/Mic92/hestia/releases/download/\(#HestiaVersion)/${hestia_asset}"
			hestia_verified=false
			for hestia_attempt in 1 2 3 4 5; do
			  rm -f "$hestia_binary"
			  if curl --proto '=https' --proto-redir '=https' -fsSL --connect-timeout 20 --max-time 120 \\
			    --retry 3 --retry-all-errors -o "$hestia_binary" "$hestia_url"; then
			    case "$hestia_platform" in
			      Darwin-*) hestia_actual="$(shasum -a 256 "$hestia_binary" | cut -d ' ' -f 1)" ;;
			      *) hestia_actual="$(sha256sum "$hestia_binary" | cut -d ' ' -f 1)" ;;
			    esac
			    if [ "$hestia_actual" = "$hestia_sha256" ]; then
			      hestia_verified=true
			      break
			    fi
			    echo "Hestia download digest $hestia_actual differs from the pinned $hestia_sha256" >&2
			  fi
			  echo "Hestia download attempt $hestia_attempt failed" >&2
			  if [ "$hestia_attempt" -lt 5 ]; then sleep $((hestia_attempt * 10)); fi
			done
			if [ "$hestia_verified" != true ]; then
			  echo "Could not download a verified Hestia \(#HestiaVersion) binary" >&2
			  exit 1
			fi
			chmod 0755 "$hestia_binary"
			"""
	}, {
		id:       "hestia.setup"
		label:    "Setup Hestia Nix Cache"
		priority: 4
		dependsOn: ["nix.install", "hestia.fetch"]
		provider: github: {
			uses: "Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527"
			with: {
				// The action copies this file instead of downloading a release.
				binary:                  "${{ runner.temp }}/hestia-release/hestia"
				"upstream-cache-filter": "true"
				"drain-timeout":         "900"
			}
		}
	}]
}
