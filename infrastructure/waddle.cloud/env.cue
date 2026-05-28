package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

let _NamespaceNix = schema.#Contributor & {
	id: "namespaceNix"
	when: runtimeType: ["nix"]
	tasks: [
		{
			id:       "nix.cache"
			label:    "Cache /nix on Namespace volume"
			priority: 0
			provider: github: {
				uses: "namespacelabs/nscloud-cache-action@v1"
				with: cache: "nix"
			}
		},
		{
			id:        "nix.chown"
			label:     "Hand /nix to the runner user"
			priority:  1
			dependsOn: ["nix.cache"]
			script:    "sudo chown -R runner /nix"
		},
		{
			id:        "nix.install"
			label:     "Install Nix"
			priority:  2
			dependsOn: ["nix.chown"]
			provider: github: {
				uses: "cachix/install-nix-action@v31"
				with: extra_nix_config: "accept-flake-config = true"
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
	ci: contributors: [_NamespaceNix, wc.#PinnedCuenvRelease]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64":    "namespace-profile-linux-x86"
			"darwin-arm64": "namespace-profile-darwin-arm64"
			amd64:          "namespace-profile-linux-x86"
		}
	}

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
					helm lint charts/livekit-sfu \
					  --set apiKeys.existingSecret=livekit-sfu-api-keys \
					  --set webhook.enabled=true \
					  --set webhook.apiKey=ci-key \
					  --set 'webhook.urls={https://xmpp.waddle.social/api/v1/livekit/webhook}' \
					  --set turn.secretName=turn-waddle-social-tls \
					  --set livekit.turn.enabled=true \
					  --set livekit.turn.domain=turn.waddle.social \
					  --set nodePorts.turnUdp.enabled=true
					helm package charts/livekit-sfu -d /tmp/charts
					chart_version="$(helm show chart charts/livekit-sfu | awk '$1 == "version:" { print $2 }')"
					if helm show chart oci://ghcr.io/waddle-social/waddle/charts/livekit-sfu --version "${chart_version}" >/dev/null 2>&1; then
					  echo "LiveKit chart ${chart_version} already exists, skipping."
					else
					  helm push /tmp/charts/livekit-sfu-*.tgz oci://ghcr.io/waddle-social/waddle/charts
					fi
				"""#]
			inputs: ["charts/livekit-sfu/**", "env.cue"]
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
			inputs: ["gitops/**", "env.cue"]
		}
	}
}
