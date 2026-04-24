package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	name: "waddle-cloud"

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	let _t = tasks

	ci: providers: ["github"]
	ci: contributors: [c.#Nix, c.#CuenvRelease]

	ci: pipelines: {
		default: {
			when: {
				branch:        ["main"]
				defaultBranch: true
				manual:        true
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
				flux push artifact oci://ghcr.io/waddle-social/waddle/gitops-livekit-sfu:latest \
				  --path=./gitops/livekit-sfu \
				  --source="$(git config --get remote.origin.url)" \
				  --revision="$(git rev-parse --short HEAD)"
			"""#]
			inputs: ["gitops/livekit-sfu/**", "gitops/kustomization-infra-livekit-sfu.yaml", "gitops/livekit-sfu-source.yaml"]
		}
	}
}
