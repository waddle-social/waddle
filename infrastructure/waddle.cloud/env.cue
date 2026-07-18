package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
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
			id:       "nix.chown"
			label:    "Hand /nix to the runner user"
			priority: 1
			dependsOn: ["nix.cache"]
			script: "sudo chown -R runner /nix"
		},
		{
			id:       "nix.install"
			label:    "Install Nix"
			priority: 2
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
	ci: contributors: [_NamespaceNix, c.#CuenvRelease]

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
			derivePaths: true
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: {
				packages:   "write"
				"id-token": "write"
			}
			tasks: [_t.helmPush, _t.gitopsPush, _t.rulesSync]
		}
		pullRequest: {
			derivePaths: true
			when: pullRequest: true
			tasks: [_t.rulesLint]
		}
	}

	tasks: {
		helmPush: schema.#Task & {
			command: "bash"
			env: {
				CI_GITHUB_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
				CI_GITHUB_ACTOR: schema.#EnvPassthrough & {name: "GITHUB_ACTOR"}
			}
			args: ["-c", #"""
					set -euo pipefail
					GITHUB_TOKEN="${CI_GITHUB_TOKEN:?missing GITHUB_TOKEN}"
					GITHUB_ACTOR="${CI_GITHUB_ACTOR:?missing GITHUB_ACTOR}"
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
		gitopsPush: schema.#Task & {
			command: "bash"
			env: {
				CI_GITHUB_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
				CI_GITHUB_ACTOR: schema.#EnvPassthrough & {name: "GITHUB_ACTOR"}
			}
			args: ["-c", #"""
					set -euo pipefail
					GITHUB_TOKEN="${CI_GITHUB_TOKEN:?missing GITHUB_TOKEN}"
					GITHUB_ACTOR="${CI_GITHUB_ACTOR:?missing GITHUB_ACTOR}"
					echo "${GITHUB_TOKEN}" | oras login ghcr.io -u "${GITHUB_ACTOR}" --password-stdin

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
		// Lint every alert-rule file (alerts-as-code, #1324). Runs on
		// every PR touching rules/** so a broken rule fails the PR,
		// not the pager.
		rulesLint: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					mimirtool rules lint rules/mimir/*.yaml
					lokitool rules lint --rule-files "$(ls rules/loki/*.yaml | paste -sd, -)"
					echo "All alert rule files lint clean."
				"""#]
			inputs: ["rules/**", "env.cue"]
		}
		// Sync the rule trees to the Grafana Cloud rulers (main push
		// only). Sync is authoritative within the `waddle` namespace;
		// see rules/README.md for the required secrets.
		rulesSync: schema.#Task & {
			command: "bash"
			env: {
				CI_MIMIR_ADDRESS: schema.#EnvPassthrough & {name: "GRAFANA_CLOUD_MIMIR_ADDRESS"}
				CI_MIMIR_TENANT:  schema.#EnvPassthrough & {name: "GRAFANA_CLOUD_MIMIR_TENANT_ID"}
				CI_LOKI_ADDRESS:  schema.#EnvPassthrough & {name: "GRAFANA_CLOUD_LOKI_ADDRESS"}
				CI_LOKI_TENANT:   schema.#EnvPassthrough & {name: "GRAFANA_CLOUD_LOKI_TENANT_ID"}
				CI_RULER_TOKEN:   schema.#EnvPassthrough & {name: "GRAFANA_CLOUD_RULER_TOKEN"}
			}
			args: ["-c", #"""
					set -euo pipefail
					: "${CI_MIMIR_ADDRESS:?missing GRAFANA_CLOUD_MIMIR_ADDRESS secret (see rules/README.md)}"
					: "${CI_MIMIR_TENANT:?missing GRAFANA_CLOUD_MIMIR_TENANT_ID secret}"
					: "${CI_LOKI_ADDRESS:?missing GRAFANA_CLOUD_LOKI_ADDRESS secret}"
					: "${CI_LOKI_TENANT:?missing GRAFANA_CLOUD_LOKI_TENANT_ID secret}"
					: "${CI_RULER_TOKEN:?missing GRAFANA_CLOUD_RULER_TOKEN secret}"
					mimirtool rules sync \
					  --address="${CI_MIMIR_ADDRESS}" \
					  --id="${CI_MIMIR_TENANT}" \
					  --key="${CI_RULER_TOKEN}" \
					  --namespaces=waddle \
					  rules/mimir/*.yaml
					lokitool rules sync \
					  --address="${CI_LOKI_ADDRESS}" \
					  --id="${CI_LOKI_TENANT}" \
					  --key="${CI_RULER_TOKEN}" \
					  --namespaces=waddle \
					  rules/loki/*.yaml
					echo "Alert rules synced to Grafana Cloud rulers."
				"""#]
			inputs: ["rules/**", "env.cue"]
		}
	}
}
