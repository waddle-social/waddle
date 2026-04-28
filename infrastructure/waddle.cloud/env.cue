package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

let _flakehubCacheContributor = schema.#Contributor & {
	id: "flakehubCache"
	tasks: [
		{
			id:       "nix.install"
			label:    "Install Determinate Nix"
			priority: 0
			provider: github: {
				uses: "DeterminateSystems/determinate-nix-action@92ffb5400c3776307a27a1727d7e2ac3dcd9f844"
				with: "extra-conf": "accept-flake-config = true"
			}
		},
		{
			id:       "flakehubCache.setup"
			label:    "Setup FlakeHub Cache"
			priority: 9
			dependsOn: ["nix.install"]
			provider: github: {
				uses: "DeterminateSystems/flakehub-cache-action@e134de896b2302c1584a7b54ff35432708607d44"
				with: {
					"flakehub-flake-name": "waddle-social/waddle"
					"use-gha-cache":       "no-preference"
				}
			}
		},
	]
}

schema.#Project & {
	name: "waddle-cloud"

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	let _t = tasks

	ci: providers: ["github"]
	ci: contributors: [_flakehubCacheContributor, c.#CuenvRelease]

	ci: pipelines: {
		default: {
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: {
				packages:   "write"
				"id-token": "write"
			}
			tasks: [_t["helm-push"], _t["gitops-push"]]
		}
	}

	tasks: {
		"helm-push": schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					echo "${GITHUB_TOKEN}" | helm registry login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
					helm lint charts/livekit-sfu --set apiKeys.values.ci=abcdefghijklmnopqrstuvwxyz123456
					helm package charts/livekit-sfu -d /tmp/charts
					chart_version="$(helm show chart charts/livekit-sfu | awk '$1 == "version:" { print $2 }')"
					if helm show chart oci://ghcr.io/waddle-social/waddle/charts/livekit-sfu --version "${chart_version}" >/dev/null 2>&1; then
					  echo "LiveKit chart ${chart_version} already exists, skipping."
					else
					  helm push /tmp/charts/livekit-sfu-*.tgz oci://ghcr.io/waddle-social/waddle/charts
					fi
				"""#]
			inputs: ["charts/**"]
		}
		"gitops-push": schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					flux push artifact oci://ghcr.io/waddle-social/waddle/gitops:latest \
					  --path=./gitops \
					  --source="$(git config --get remote.origin.url)" \
					  --revision="$(git rev-parse --short HEAD)"

					flux push artifact oci://ghcr.io/waddle-social/waddle/gitops-livekit-sfu:latest \
					  --path=./gitops/livekit-sfu \
					  --source="$(git config --get remote.origin.url)" \
					  --revision="$(git rev-parse --short HEAD)"
				"""#]
			inputs: ["gitops/**"]
		}
	}
}
