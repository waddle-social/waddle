package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

schema.#Project & {
	name: "waddle-cloud"

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	let _t = tasks

	ci: providers: ["github"]
	ci: contributors: [wc.#Nix, wc.#Hestia, c.#CuenvRelease, c.#OnePassword]

	// Grafana Cloud ruler credentials for rulesSync (#1324). Resolved
	// via 1Password on main-push CI (the OnePassword contributor +
	// pipeline environment), mirroring chat's Cloudflare/Faro pattern.
	// The operator creates the item once; see rules/README.md.
	env: {
		environment: production: {
			GRAFANA_CLOUD_MIMIR_ADDRESS: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Alerting/mimir-address"
			}
			GRAFANA_CLOUD_MIMIR_TENANT_ID: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Alerting/mimir-tenant-id"
			}
			GRAFANA_CLOUD_LOKI_ADDRESS: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Alerting/loki-address"
			}
			GRAFANA_CLOUD_LOKI_TENANT_ID: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Alerting/loki-tenant-id"
			}
			GRAFANA_CLOUD_RULER_TOKEN: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Alerting/ruler-token"
			}
			GRAFANA_CLOUD_URL: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Dashboards/url"
			}
			GRAFANA_CLOUD_DASHBOARDS_TOKEN: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Cloud-Dashboards/service-account-token"
			}
		}
	}

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
			environment: "production"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: {
				packages:   "write"
				"id-token": "write"
			}
			tasks: [_t.helmPush, _t.gitopsPush, _t.deployAnnotation, _t.rulesSync, _t.dashboardsSync]
		}
		pullRequest: {
			derivePaths: true
			when: pullRequest: true
			tasks: [_t.rulesLint, _t.dashboardsLint]
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
		// Record the GitOps artifact push as a Grafana organization
		// annotation. Flux reconciliation is asynchronous, so this marks the
		// rollout handoff rather than claiming the pods are already ready.
		// Sequenced after both pushes so a failed push never gets a marker.
		deployAnnotation: schema.#Task & {
			dependsOn: [helmPush, gitopsPush]
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					: "${GRAFANA_CLOUD_URL:?missing — create the Grafana-Cloud-Dashboards 1Password item (see dashboards/README.md)}"
					: "${GRAFANA_CLOUD_DASHBOARDS_TOKEN:?missing 1Password field service-account-token}"
					grafana_url="${GRAFANA_CLOUD_URL%/}"
					revision="$(git rev-parse --short HEAD)"
					timestamp_ms="$(( $(date +%s) * 1000 ))"
					annotation_response="$(mktemp)"
					trap 'rm -f "${annotation_response}"' EXIT
					annotation_status="$(jq -n \
					  --arg text "waddle infra deploy ${revision}" \
					  --argjson time "${timestamp_ms}" \
					  '{text: $text, tags: ["deploy", "waddle"], time: $time}' \
					  | curl -sS -o "${annotation_response}" -w '%{http_code}' \
					      -X POST "${grafana_url}/api/annotations" \
					      -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}" \
					      -H 'Content-Type: application/json' \
					      --data-binary @-)"
					case "${annotation_status}" in
					  2??) ;;
					  *)
					    echo "Grafana annotation creation failed with HTTP ${annotation_status}:" >&2
					    cat "${annotation_response}" >&2
					    exit 1
					    ;;
					esac
					echo "Grafana deploy annotation posted for ${revision}."
				"""#]
			inputs: ["gitops/**", "charts/**", "env.cue"]
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
		// Validate every dashboard document on PRs. The skeleton exemption is
		// declared in the dashboard's own tags rather than hidden in CI logic.
		dashboardsLint: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					./scripts/validate-dashboards.sh
				"""#]
			inputs: ["dashboards/**", "scripts/validate-dashboards.sh", "env.cue"]
		}
		// Sync the rule trees to the Grafana Cloud rulers (main push
		// only). Sync is authoritative within the `waddle` namespace;
		// see rules/README.md for the required secrets.
		rulesSync: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					: "${GRAFANA_CLOUD_MIMIR_ADDRESS:?missing — create the Grafana-Cloud-Alerting 1Password item (see rules/README.md)}"
					: "${GRAFANA_CLOUD_MIMIR_TENANT_ID:?missing 1Password field mimir-tenant-id}"
					: "${GRAFANA_CLOUD_LOKI_ADDRESS:?missing 1Password field loki-address}"
					: "${GRAFANA_CLOUD_LOKI_TENANT_ID:?missing 1Password field loki-tenant-id}"
					: "${GRAFANA_CLOUD_RULER_TOKEN:?missing 1Password field ruler-token}"
					mimirtool rules sync \
					  --address="${GRAFANA_CLOUD_MIMIR_ADDRESS}" \
					  --id="${GRAFANA_CLOUD_MIMIR_TENANT_ID}" \
					  --key="${GRAFANA_CLOUD_RULER_TOKEN}" \
					  --namespaces=waddle \
					  rules/mimir/*.yaml
					lokitool rules sync \
					  --address="${GRAFANA_CLOUD_LOKI_ADDRESS}" \
					  --id="${GRAFANA_CLOUD_LOKI_TENANT_ID}" \
					  --key="${GRAFANA_CLOUD_RULER_TOKEN}" \
					  --namespaces=waddle \
					  rules/loki/*.yaml
					echo "Alert rules synced to Grafana Cloud rulers."
				"""#]
			inputs: ["rules/**", "env.cue"]
		}
		// Publish dashboards to the stable Waddle folder. Folder lookup plus
		// stable dashboard UIDs and overwrite=true make repeated syncs safe.
		dashboardsSync: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					: "${GRAFANA_CLOUD_URL:?missing — create the Grafana-Cloud-Dashboards 1Password item (see dashboards/README.md)}"
					: "${GRAFANA_CLOUD_DASHBOARDS_TOKEN:?missing 1Password field service-account-token}"
					grafana_url="${GRAFANA_CLOUD_URL%/}"
					folder_response="$(mktemp)"
					trap 'rm -f "${folder_response}"' EXIT

					# Resolve the folder BEFORE ever creating it, in three steps:
					# 1) by its stable uid; 2) by unique title in the folder
					# listing; 3) create it. Ensure-by-create cannot lead here:
					# a create colliding on uid answers 412 version-mismatch
					# (409 only covers title collisions), and Grafana Cloud has
					# been observed serving folders that list fine but 404 on
					# the by-uid endpoint (the post-merge #1327 sync failed on
					# exactly that), which only the listing fallback survives.
					folder_uid=""
					folder_lookup_status="$(curl -sS -o "${folder_response}" -w '%{http_code}' \
					  "${grafana_url}/api/folders/uid/waddle" \
					  -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}")"
					if [ "${folder_lookup_status}" = "200" ]; then
					  folder_uid="$(jq -er '.uid' "${folder_response}")"
					else
					  # Fall back to the listing: first by the stable uid (covers a
					  # renamed title and the observed list-ok/uid-endpoint-404
					  # state), then by unique title. Several folders titled
					  # Waddle is an ambiguity a human must resolve — creating
					  # yet another folder would strand the existing ones.
					  curl --fail-with-body -sS "${grafana_url}/api/folders" \
					    -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}" \
					    > "${folder_response}"
					  folder_uid="$(jq -r \
					    '[.[] | select(.uid == "waddle")] | if length == 1 then .[0].uid else "" end' \
					    "${folder_response}")"
					  if [ -z "${folder_uid}" ]; then
					    title_matches="$(jq -r '[.[] | select(.title == "Waddle")] | length' "${folder_response}")"
					    if [ "${title_matches}" -gt 1 ]; then
					      echo "Refusing to sync: ${title_matches} folders titled \"Waddle\" and none with uid \"waddle\"; resolve the duplicates manually." >&2
					      exit 1
					    fi
					    if [ "${title_matches}" = "1" ]; then
					      folder_uid="$(jq -er '[.[] | select(.title == "Waddle")] | .[0].uid' "${folder_response}")"
					    fi
					  fi
					fi
					if [ -z "${folder_uid}" ]; then
					  folder_status="$(curl -sS -o "${folder_response}" -w '%{http_code}' \
					    -X POST "${grafana_url}/api/folders" \
					    -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}" \
					    -H 'Content-Type: application/json' \
					    --data '{"uid":"waddle","title":"Waddle"}')"
					  case "${folder_status}" in
					    200|201)
					      folder_uid="$(jq -er '.uid' "${folder_response}")"
					      ;;
					    *)
					      echo "Grafana folder creation failed with HTTP ${folder_status}:" >&2
					      cat "${folder_response}" >&2
					      exit 1
					      ;;
					  esac
					fi

					for dashboard_file in dashboards/*.json; do
					  dashboard_status="$(jq -c --arg folder_uid "${folder_uid}" \
					    '{dashboard: (. + {id: null}), folderUid: $folder_uid, overwrite: true}' \
					    "${dashboard_file}" \
					    | curl -sS -o "${folder_response}" -w '%{http_code}' \
					        -X POST "${grafana_url}/api/dashboards/db" \
					        -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}" \
					        -H 'Content-Type: application/json' \
					        --data-binary @-)"
					  case "${dashboard_status}" in
					    2??) ;;
					    *)
					      echo "Grafana dashboard sync failed for ${dashboard_file} with HTTP ${dashboard_status}:" >&2
					      cat "${folder_response}" >&2
					      exit 1
					      ;;
					  esac
					  echo "Synced ${dashboard_file} to Grafana folder Waddle."
					done
					# The repo is the single source of truth: any dashboard left in
					# the Waddle folder whose uid no longer exists in
					# dashboards/*.json was deleted or renamed in source control and
					# is pruned.
					local_uids="$(jq -r '.uid' dashboards/*.json)"
					# The jq folderUid re-check is deliberate defense-in-depth for
					# this destructive loop: if the server ever ignored the
					# folderUIDs query parameter, the client-side filter still
					# confines the prune to the Waddle folder.
					remote_uids="$(curl --fail-with-body -sS \
					  "${grafana_url}/api/search?type=dash-db&folderUIDs=${folder_uid}&limit=1000" \
					  -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}" \
					  | jq -r --arg folder_uid "${folder_uid}" \
					      '.[] | select(.folderUid == $folder_uid) | .uid')"
					# validate-dashboards.sh enforces this charset for every
					# repo-owned uid, so this skip can only fire for dashboards
					# created outside this pipeline.
					for remote_uid in ${remote_uids}; do
					  if [[ ! "${remote_uid}" =~ ^[A-Za-z0-9_-]+$ ]]; then
					    echo "Skipping prune of dashboard with unexpected uid ${remote_uid@Q} (not URL-safe)." >&2
					    continue
					  fi
					  if ! grep -qx "${remote_uid}" <<< "${local_uids}"; then
					    delete_status="$(curl -sS -o "${folder_response}" -w '%{http_code}' \
					      -X DELETE "${grafana_url}/api/dashboards/uid/${remote_uid}" \
					      -H "Authorization: Bearer ${GRAFANA_CLOUD_DASHBOARDS_TOKEN}")"
					    case "${delete_status}" in
					      2??) echo "Pruned dashboard ${remote_uid} (no longer in source control)." ;;
					      *)
					        echo "Failed to prune dashboard ${remote_uid} with HTTP ${delete_status}:" >&2
					        cat "${folder_response}" >&2
					        exit 1
					        ;;
					    esac
					  fi
					done
					echo "All dashboards synced to Grafana Cloud."
				"""#]
			inputs: ["dashboards/**", "env.cue"]
		}
	}
}
