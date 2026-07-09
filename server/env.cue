package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	xRust "github.com/cuenv/cuenv/contrib/rust"
)

let _rustInputs = [
	"Cargo.toml",
	"Cargo.lock",
	"rust-toolchain.toml",
	"crates/**",
	"extensions/**",
	"scripts/**",
	"wit/**",
]

let _nixInputs = [
	"../flake.nix",
	"../flake.lock",
	"Cargo.toml",
	"Cargo.lock",
	"rust-toolchain.toml",
	"crates/**",
	"extensions/**",
	"wit/**",
]

let _chartInputs = ["charts/waddle-server/**"]
let _gitopsWaddleServerInputs = [
	"../infrastructure/waddle.cloud/gitops/waddle-server/**",
	"../infrastructure/waddle.cloud/gitops/waddle-server-source.yaml",
	"../infrastructure/waddle.cloud/gitops/kustomization-infra-waddle-server.yaml",
]
let _deploymentInputs = ["deployment.cue"]
let _releasePublicationInputs = [
	"scripts/publish-waddle-release.sh",
	"scripts/publish-container-image.sh",
	"scripts/publish-helm-chart.sh",
	"scripts/publish-extension-modules.sh",
	"scripts/publish-gitops-release.sh",
	"scripts/publish-container-tags.sh",
]
let _gateZeroEvidenceSourceInputs = [
	"capabilities.toml",
	"disco-target-contract.json",
	"crates/waddle-server/src/build_identity.rs",
	"crates/waddle-server/src/notification_outbox/**",
	"crates/waddle-server/src/pending_delivery/**",
	"crates/waddle-server/src/server/**",
	"crates/waddle-server/src/telemetry.rs",
	"crates/waddle-xmpp/src/admin.rs",
	"crates/waddle-xmpp/src/auth/**",
	"crates/waddle-xmpp/src/disco/**",
	"crates/waddle-xmpp/src/isr.rs",
	"crates/waddle-xmpp/src/parser/ns.rs",
	"crates/waddle-xmpp/src/prometheus.rs",
	"crates/waddle-xmpp/src/prometheus/**",
	"crates/waddle-xmpp/src/protocol/frame.rs",
	"crates/waddle-xmpp/src/protocol/phase.rs",
	"crates/waddle-xmpp/src/pubsub/pep.rs",
	"crates/waddle-xmpp/src/registry/**",
	"crates/waddle-xmpp/src/stream_management/**",
	"crates/waddle-xmpp/src/protocol/handlers/version.rs",
	"crates/waddle-xmpp/src/xep/xep0430.rs",
	"crates/waddle-xmpp/src/xep/xep0433.rs",
	"crates/waddle-xmpp/src/xep/xep0092.rs",
	"crates/waddle-xmpp-client/src/bin/waddle_capability_collector.rs",
	"crates/waddle-xmpp-client/src/client.rs",
	"crates/waddle-xmpp-client/src/capability_evidence.rs",
	"crates/waddle-xmpp-client/src/capability_evidence/**",
	"crates/waddle-xmpp-client/src/config.rs",
	"crates/waddle-xmpp-client/src/discovery.rs",
	"crates/waddle-xmpp-client/src/discovery/**",
	"crates/waddle-xmpp-client/src/error.rs",
	"crates/waddle-xmpp-client/src/event.rs",
	"crates/waddle-xmpp-client/src/runtime.rs",
	"crates/waddle-xmpp-client/src/runtime/**",
	"crates/waddle-xmpp-client/src/transport.rs",
	"crates/waddle-xmpp-client/src/transport/**",
	"crates/waddle-xmpp-core/src/build_identity.rs",
	"crates/waddle-xmpp-core/src/disco_target.rs",
	"crates/waddle-xmpp-core/src/disco/**",
	"crates/waddle-xmpp-core/src/pubsub/pep.rs",
	"charts/waddle-server/templates/configmap.yaml",
	"charts/waddle-server/templates/deployment.yaml",
	"charts/waddle-server/templates/_helpers.tpl",
	"charts/waddle-server/templates/service.yaml",
	"charts/waddle-server/templates/validations.yaml",
	"charts/waddle-server/values.yaml",
	"../infrastructure/waddle.cloud/gitops/grafana-alloy/helmrelease.yaml",
	"../docs/observability/**",
	"../chat/astro.config.mjs",
	"../chat/scripts/build-identity.mjs",
	"../chat/scripts/generate-build-identity.mjs",
	"../chat/scripts/resolve-commit-sha.mjs",
	"../chat/src/auth/**",
	"../chat/src/build-identity-contract.ts",
	"../chat/src/build-identity.ts",
	"../chat/src/layouts/AppLayout.astro",
	"../chat/src/lib/telemetry.ts",
	"../chat/src/lib/telemetry/**",
	"../chat/src/lib/xmpp/**",
]
let _capabilityCollectionEnv = {
	WADDLE_CAPABILITY_ENDPOINT: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_ACCOUNT_JID: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_REPRESENTATIVE_MUC_ROOM: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_ACCESS_TOKEN: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_XMPP_DOMAIN: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_MUC_DOMAIN: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_SPACES_DOMAIN: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_SERVER_COMMIT: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_WINDOW_START: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_WINDOW_END: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_JOB: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_ENVIRONMENT: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_CLUSTER: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_NAMESPACE: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_EXPECTED_REPLICAS: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_ORIGIN: schema.#EnvPassthrough & {}
}
let _baselineFinalizationEnv = {
	WADDLE_CAPABILITY_SERVER_COMMIT: schema.#EnvPassthrough & {}
	WADDLE_BASELINE_WEB_COMMIT: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_WINDOW_START: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_WINDOW_END: schema.#EnvPassthrough & {}
	WADDLE_BASELINE_CAPTURED_AT: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_JOB: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_ENVIRONMENT: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_CLUSTER: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_NAMESPACE: schema.#EnvPassthrough & {}
	WADDLE_CAPABILITY_EXPECTED_REPLICAS: schema.#EnvPassthrough & {}
}

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
	name: "waddle-server"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	let _t = tasks

	ci: providers: ["github"]
	ci: contributors: [
		_NamespaceNix,
		c.#CuenvRelease,
		c.#OnePassword,
		schema.#Contributor & {
			id: "flakehub"
			when: environment: ["flakehub"]
			tasks: [
				{
					id:       "checkout.tag"
					label:    "Checkout tag"
					priority: 1
					provider: github: {
						uses: "actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd"
						with: {
							"persist-credentials": false
							ref:                   "${{ (inputs.tag != null) && format('refs/tags/{0}', inputs.tag) || '' }}"
						}
					}
				},
				{
					id:       "push"
					label:    "Push to FlakeHub"
					priority: 3
					dependsOn: ["checkout.tag", "nix.install"]
					provider: github: {
						uses: "DeterminateSystems/flakehub-push@a225170f3ab20a9d93dbb4424090ef0523ca7425"
						with: {
							visibility:             "public"
							name:                   "waddle-social/waddle"
							tag:                    "${{ inputs.tag }}"
							"include-output-paths": true
						}
					}
				},
			]
		},
	]

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
			mode: "expanded"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: {
				permissions: {
					"id-token":      "write"
					packages:        "write"
					"pull-requests": "none"
				}
			}
			tasks: [
				_t.checkRootSyncDrift,
				_t.checkCiDrift,
				_t.checkSwitchableAlternativeProgram,
				_t.fmt,
				_t.clippy,
				_t.test,
				_t.checkXmppClientFfiBindings,
				_t.renderDeployment,
				_t.buildExtensionModules,
				_t.publishContainerImage,
			]
		}
		"Publish tags to FlakeHub": {
			environment: "flakehub"
			when: {
				tag: ["v?[0-9]+.[0-9]+.[0-9]+*"]
				manual: tag: {
					description: "The existing tag to publish to FlakeHub"
					type:        "string"
					required:    true
				}
			}
			provider: github: permissions: {
				"id-token":      "write"
				contents:        "read"
				"pull-requests": "none"
			}
			tasks: [_t.flakehubPublished]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			mode: "expanded"
			provider: github: permissions: {
				"id-token":      "write"
				contents:        "read"
				checks:          "write"
				packages:        "read"
				"pull-requests": "none"
			}
			tasks: [_t.checkRootSyncDrift, _t.checkCiDrift, _t.checkSwitchableAlternativeProgram, _t.nixFmt, _t.nixClippy, _t.nixTest, _t.checkXmppClientFfiBindings, _t.renderDeployment, _t.nixBuildExtensionModules, _t.nixBuildCi]
		}
		xmppCompliance: {
			mode: "expanded"
			when: {
				branch: ["main"]
				defaultBranch: true
				pullRequest:   true
				manual:        true
			}
			provider: github: permissions: {
				"id-token":      "write"
				contents:        "read"
				checks:          "write"
				packages:        "read"
				"pull-requests": "none"
			}
			tasks: [
				_t.nixXmppUnitTests,
				_t.nixXmppServerTests,
				_t.nixXmppXepIntegration,
			]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			command: "cuenv"
			args: ["sync", "ci", "--check", "-A"]
			inputs: [
				"**/env.cue",
				"deployment.cue",
				"../.github/workflows/waddle-server-*.yml",
			]
		}

		checkRootSyncDrift: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					cuenv sync --check -p ..
					git diff --exit-code -- ../.gitignore ../cuenv.lock
				"""#]
			inputs: [
				"../env.cue",
				"../cuenv.lock",
				"../.rules.cue",
				"../.gitignore",
			]
		}

		checkSwitchableAlternativeProgram: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					tracker=../docs/planning/switchable-alternative.md
					test -f capabilities.toml
					test -f ../docs/product/critical-journeys.json
					test -f ../docs/product/gate-evidence.json
					test -f ../docs/observability/switchable-baseline-signals.json
					test -f ../docs/runbooks/switchable-baseline.md
					test -f ../scripts/collect-switchable-baseline.ts
					test -f ../scripts/finalize-switchable-baseline.ts
					for gate in 0 1 2 3 4 5; do
					  test "$(grep -c "^## Gate ${gate} —" "${tracker}")" -eq 1
					done
					test "$(grep -Ec '^\| [0-5] \| (planned|in-progress|blocked|complete) \|' "${tracker}")" -eq 6
					test "$(grep -Ec '^\*\*Current gate: (Gate [0-5]|Program complete)\*\*$' "${tracker}")" -eq 1
					journey_status="$(bun -e 'const contract = await Bun.file("../docs/product/critical-journeys.json").json(); console.log(contract.journeyStatus)')"
					if grep -q '^\*\*Current gate: Program complete\*\*$' "${tracker}"; then
					  test "$(grep -Ec '^\| [0-5] \| complete \|' "${tracker}")" -eq 6
					  test "$(grep -Ec '^\| [0-5] \| in-progress \|' "${tracker}")" -eq 0
					  test "${journey_status}" = ready
					else
					  test "$(grep -Ec '^\| [0-5] \| in-progress \|' "${tracker}")" -eq 1
					  current_gate="$(grep '^\*\*Current gate: Gate [0-5]\*\*$' "${tracker}" | grep -Eo '[0-5]')"
					  grep -q "^| ${current_gate} | in-progress |" "${tracker}"
					  for gate in 0 1 2 3 4 5; do
					    status="$(grep "^| ${gate} |" "${tracker}" | cut -d '|' -f 3 | xargs)"
					    if [ "${gate}" -lt "${current_gate}" ]; then
					      test "${status}" = complete
					    elif [ "${gate}" -gt "${current_gate}" ]; then
					      test "${status}" = planned -o "${status}" = blocked
					    fi
					  done
					fi
					for gate in 2 3 4; do
					  tracker_status="$(grep "^| ${gate} |" "${tracker}" | cut -d '|' -f 3 | xargs)"
					  evidence_status="$(GATE="${gate}" bun -e 'const contract = await Bun.file("../docs/product/critical-journeys.json").json(); console.log(contract.gateReadiness[process.env.GATE])')"
					  if [ "${tracker_status}" = complete ]; then
					    test "${evidence_status}" = ready
					  elif [ "${evidence_status}" = ready ]; then
					    echo "Gate ${gate} journey evidence cannot be ready before the tracker gate is complete" >&2
					    exit 1
					  fi
					done
					for gate in 0 1 5; do
					  tracker_status="$(grep "^| ${gate} |" "${tracker}" | cut -d '|' -f 3 | xargs)"
					  evidence_status="$(GATE="${gate}" bun -e 'const ledger = await Bun.file("../docs/product/gate-evidence.json").json(); console.log(ledger.gates[process.env.GATE].status)')"
					  if [ "${tracker_status}" = complete ]; then
					    test "${evidence_status}" = ready
					  elif [ "${evidence_status}" = ready ]; then
					    echo "Gate ${gate} evidence cannot be ready before the tracker gate is complete" >&2
					    exit 1
					  fi
					done
					while IFS= read -r row; do
					  echo "${row}" | grep -Eq '\[[^]]+\]\([^)]+\)'
					  if echo "${row}" | grep -Eq 'pending|[[:space:]][—-][[:space:]]'; then
					    echo "completed program gates must link durable non-pending evidence" >&2
					    exit 1
					  fi
					done < <(grep '^| [0-5] | complete |' "${tracker}" || true)
					bun test ../tests
				"""#]
			inputs: [
				"../flake.nix",
				"../apps/apple/**/Tests/**",
				"../chat/src/auth/session.ts",
				"../chat/src/lib/telemetry.ts",
				"../chat/src/lib/xmpp/**",
				"../chat/tests/**",
				"../docs/evidence/**",
				"../docs/observability/**",
				"../docs/runbooks/**",
				"capabilities.toml",
				"disco-target-contract.json",
				"crates/waddle-xmpp-client/src/bin/waddle_capability_collector.rs",
				"crates/waddle-xmpp-client/src/capability_evidence.rs",
				"crates/waddle-xmpp-client/src/capability_evidence/**",
				"crates/waddle-xmpp-client/src/discovery/**",
				"crates/waddle-server/src/build_identity.rs",
				"crates/waddle-server/src/main.rs",
				"crates/waddle-server/src/server/disco_targets.rs",
				"crates/waddle-server/src/server/health.rs",
				"crates/waddle-server/src/server/room_registry_gauge.rs",
				"crates/waddle-server/src/server/trace.rs",
				"crates/waddle-server/src/server/xmpp_auth_state.rs",
				"crates/waddle-server/src/server/routes/interpret/direct_archive.rs",
				"crates/waddle-server/src/server/routes/interpret/groupchat_archive.rs",
				"crates/waddle-server/src/server/routes/websocket/frame.rs",
				"crates/waddle-server/src/server/routes/websocket/parse_errors.rs",
				"crates/waddle-server/src/server/routes/websocket/sasl.rs",
				"crates/waddle-server/src/server/routes/websocket/transport_xml.rs",
				"crates/waddle-server/src/telemetry.rs",
				"crates/waddle-xmpp/src/auth/mod.rs",
				"crates/waddle-xmpp/src/auth/scram/**",
				"crates/waddle-xmpp/src/auth/scram.rs",
				"crates/waddle-xmpp/src/prometheus/**",
				"crates/waddle-xmpp/src/prometheus.rs",
				"crates/waddle-xmpp/src/protocol/frame.rs",
				"crates/waddle-xmpp/src/protocol/phase.rs",
				"crates/waddle-xmpp/src/auth/oauthbearer.rs",
				"crates/**/tests/**",
				"crates/waddle-server/src/server/routes/websocket/tests/**",
				"crates/waddle-server/tests/server_capability_manifest.rs",
				"../docs/product/critical-journeys.json",
				"../docs/product/gate-evidence.json",
				"../docs/product/performance-profile.json",
				"../docs/planning/switchable-alternative.md",
				"../scripts/collect-switchable-baseline.ts",
				"../scripts/finalize-switchable-baseline.ts",
				"../scripts/switchable-baseline/**",
				"../tests/**",
				"../infrastructure/waddle.cloud/gitops/grafana-alloy/helmrelease.yaml",
				"charts/waddle-server/templates/deployment.yaml",
				"charts/waddle-server/templates/validations.yaml",
				"charts/waddle-server/values.yaml",
				"../cuenv.lock",
				"../xeps/xep-*.xml",
				"env.cue",
			]
		}

		collectCapabilityBaseline: schema.#Task & {
			command: "bash"
			args: ["scripts/collect-capability-baseline.sh"]
			env: _capabilityCollectionEnv
			cache: mode: "never"
			inputs: [
				"Cargo.toml",
				"Cargo.lock",
				"crates/waddle-xmpp-client/**",
				"disco-target-contract.json",
				"scripts/collect-capability-baseline.sh",
			]
			outputs: ["../target/switchable-baseline-inputs/capability/live-disco-export.json"]
		}

		finalizeGateZeroBaseline: schema.#Task & {
			command: "bash"
			args: ["scripts/finalize-gate-zero-baseline.sh"]
			env: _baselineFinalizationEnv
			cache: mode: "never"
			inputs: list.Concat([[
				"../target/switchable-baseline-inputs/capability/live-disco-export.json",
				"../target/switchable-baseline-inputs/prometheus/telemetry-baseline.json",
				"../target/switchable-baseline-inputs/faro/browser-auth-bootstrap.json",
				"../target/switchable-baseline-inputs/faro/browser-message-ack-latency.json",
				"../target/switchable-baseline-inputs/faro/browser-session-lifecycle.json",
				"../target/switchable-baseline-inputs/faro/browser-reconnect-duration.json",
				"../target/switchable-baseline-inputs/attestation/live-collection-subject.json",
				"../target/switchable-baseline-inputs/attestation/live-collection.sigstore.json",
				"scripts/finalize-gate-zero-baseline.sh",
				"../scripts/finalize-switchable-baseline.ts",
				"../scripts/switchable-baseline/**",
			], _gateZeroEvidenceSourceInputs])
			outputs: ["../docs/evidence/gate-0/**"]
		}

		verifySwitchableBaseline: schema.#Task & {
			command: "bun"
			args: ["../scripts/finalize-switchable-baseline.ts", "verify"]
			cache: mode: "never"
			inputs: list.Concat([[
				"../docs/evidence/gate-0/**",
				"../scripts/finalize-switchable-baseline.ts",
				"../scripts/switchable-baseline/**",
			], _gateZeroEvidenceSourceInputs])
		}

		nixFmt: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-fmt"]
			inputs: _nixInputs
		}

		nixClippy: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-clippy"]
			inputs: _nixInputs
		}

		nixTest: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-test"]
			inputs: _nixInputs
		}

		nixBuildCi: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-ci-build"]
			inputs: _nixInputs
		}

		nixBuildExtensionModules: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-extension-modules"]
			inputs: _nixInputs
		}

		fmt: xRust.#Fmt & {
			args: ["fmt", "--all", "--", "--check"]
			inputs: _rustInputs
		}

		clippy: xRust.#Clippy & {
			args: ["clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]
			inputs: _rustInputs
		}

		test: xRust.#Test & {
			args: ["test", "--workspace", "--all-targets", "--locked"]
			inputs: _rustInputs
		}

		checkXmppClientFfiBindings: schema.#Task & {
			command: "bash"
			args: ["scripts/check-xmpp-client-ffi-bindings.sh"]
			inputs: list.Concat([
				_rustInputs,
				[
					"../apps/apple/Waddle/RustClient/Generated/**",
				],
			])
		}

		buildCi: xRust.#Build & {
			args: ["build", "--profile", "ci", "--locked", "--package", "waddle-server"]
			inputs: _rustInputs
			outputs: ["target/ci/waddle-server"]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test]
		}

		buildRelease: xRust.#Build & {
			args: ["build", "--release", "--locked", "--package", "waddle-server"]
			inputs: _rustInputs
			outputs: ["target/release/waddle-server"]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test]
		}

		buildExtensionModules: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					rustup target add wasm32-wasip2 >/dev/null 2>&1 || true
					for module in link-board ai-chatbot decision-polls github stargate-quotes; do
					  cargo build --release --locked --target wasm32-wasip2 --target-dir target --manifest-path "extensions/${module}/Cargo.toml"
					done
					test -s target/wasm32-wasip2/release/link_board.wasm
					test -s target/wasm32-wasip2/release/ai_chatbot.wasm
					test -s target/wasm32-wasip2/release/decision_polls.wasm
					test -s target/wasm32-wasip2/release/github.wasm
					test -s target/wasm32-wasip2/release/stargate_quotes.wasm
				"""#]
			inputs: _rustInputs
			outputs: [
				"server/target/wasm32-wasip2/release/link_board.wasm",
				"server/target/wasm32-wasip2/release/ai_chatbot.wasm",
				"server/target/wasm32-wasip2/release/decision_polls.wasm",
				"server/target/wasm32-wasip2/release/github.wasm",
				"server/target/wasm32-wasip2/release/stargate_quotes.wasm",
			]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test]
		}

		renderDeployment: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail

					placeholder_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"
					sample_digest="sha256:1111111111111111111111111111111111111111111111111111111111111111"
					sample_git_sha="1111111111111111111111111111111111111111"
					chart_render="$(mktemp)"
					gitops_values="$(mktemp).yaml"
					gitops_render="$(mktemp)"
					gitops_kustomize="$(mktemp)"
					modules_yaml="$(mktemp)"
					published_values="$(mktemp).yaml"
					published_render="$(mktemp)"
					published_gitops_dir="$(mktemp -d)"
					published_gitops_kustomize="$(mktemp)"
					chart_secret_args=(
					  --set-string secret.sessionKey=ci-session-key
					  --set-string secret.occupantIdSecret=ci-occupant-id-secret-32-bytes-long
					)
					chart_version="$(yq -r '.version' charts/waddle-server/Chart.yaml)"
					chart_source=../infrastructure/waddle.cloud/gitops/waddle-server/chart-ocirepository.yaml
					gitops_chart_tag="$(yq -r '.spec.ref.tag' "${chart_source}")"
					if [ "${gitops_chart_tag}" != "${chart_version}" ]; then
					  echo "checked-in chart OCIRepository must select exact chart version ${chart_version}, got ${gitops_chart_tag}" >&2
					  exit 1
					fi
					yq -e '.kind == "OCIRepository" and .metadata.name == "waddle-server-chart" and .metadata.namespace == "waddle"' "${chart_source}" >/dev/null
					yq -e '.spec.url == "oci://ghcr.io/waddle-social/waddle/charts/waddle-server"' "${chart_source}" >/dev/null
					yq -e '.spec.layerSelector.mediaType == "application/vnd.cncf.helm.chart.content.v1.tar+gzip" and .spec.layerSelector.operation == "copy"' "${chart_source}" >/dev/null
					yq -e '.spec.chartRef.kind == "OCIRepository" and .spec.chartRef.name == "waddle-server-chart" and .spec.chartRef.namespace == "waddle" and (.spec.chart == null)' ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml >/dev/null

					cue vet .
					cue vet . ../infrastructure/waddle.cloud/gitops/waddle-server/runtime-external-secret.yaml -d '#RuntimeExternalSecret'
					cue vet . ../infrastructure/waddle.cloud/gitops/waddle-server/openrouter-external-secret.yaml -d '#OpenRouterExternalSecret'
					cue vet . ../infrastructure/waddle.cloud/gitops/waddle-server/spicedb-config-external-secret.yaml -d '#SpiceDbExternalSecret'
					for manifest in ../infrastructure/waddle.cloud/gitops/waddle-server/*external-secret.yaml; do
					  cue vet . "${manifest}" -d '#ExternalSecretNoDefaultPassword'
					done

					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false > /dev/null 2>&1; then
					  echo "chart must require an operator-owned WADDLE_SESSION_KEY and WADDLE_OCCUPANT_ID_SECRET source" >&2
					  exit 1
					fi
					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  --set secret.create=false \
					  "${chart_secret_args[@]}" > /dev/null 2>&1; then
					  echo "chart must reject inline runtime secrets when secret.create=false because no Secret is rendered" >&2
					  exit 1
					fi
					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  --set-string 'extraSecretRefs[0]=not-runtime-secrets' > /dev/null 2>&1; then
					  echo "chart must reject arbitrary extraSecretRefs as proof of required runtime keys" >&2
					  exit 1
					fi

					helm lint charts/waddle-server --set spicedb.enabled=false "${chart_secret_args[@]}"
					helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  "${chart_secret_args[@]}" > "${chart_render}"
					liveness_path="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | .livenessProbe.httpGet.path' "${chart_render}")"
					case "${liveness_path}" in
					  /health) ;;
					  *) echo "chart liveness probe must use /health, got: ${liveness_path:-<missing>}" >&2; exit 1 ;;
					esac
					readiness_path="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | .readinessProbe.httpGet.path' "${chart_render}")"
					case "${readiness_path}" in
					  /readyz) ;;
					  *) echo "chart readiness probe must use the self-fencing-aware /readyz endpoint, got: ${readiness_path:-<missing>}" >&2; exit 1 ;;
					esac
					for env_and_path in \
					  "OTEL_SERVICE_INSTANCE_ID:metadata.uid" \
					  "K8S_POD_UID:metadata.uid" \
					  "K8S_POD_NAME:metadata.name" \
					  "K8S_NAMESPACE_NAME:metadata.namespace"; do
					  env_name="${env_and_path%%:*}"
					  field_path="${env_and_path#*:}"
					  yq -e "select(.kind == \"Deployment\") | .spec.template.spec.containers[] | select(.name == \"waddle-server\") | (.env // [])[] | select(.name == \"${env_name}\" and .valueFrom.fieldRef.fieldPath == \"${field_path}\")" "${chart_render}" > /dev/null
					done

					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  "${chart_secret_args[@]}" \
					  --set-string telemetry.environment='Production/Secret' > /dev/null 2>&1; then
					  echo "chart must reject unsafe deployment scope labels" >&2
					  exit 1
					fi
					for reserved_identity in WADDLE_GIT_SHA OTEL_SERVICE_VERSION; do
					  if helm template waddle-server charts/waddle-server \
					    --namespace waddle \
					    --set spicedb.enabled=false \
					    "${chart_secret_args[@]}" \
					    --set-string "containerExtraEnv[0].name=${reserved_identity}" \
					    --set-string 'containerExtraEnv[0].value=ffffffffffffffffffffffffffffffffffffffff' > /dev/null 2>&1; then
					    echo "chart must reject containerExtraEnv override of ${reserved_identity}" >&2
					    exit 1
					  fi
					done
					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  "${chart_secret_args[@]}" \
					  --set-string 'config.extraEnv.OTEL_SERVICE_VERSION=spoofed' > /dev/null 2>&1; then
					  echo "chart must reject config.extraEnv overrides of immutable telemetry identity" >&2
					  exit 1
					fi
					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  "${chart_secret_args[@]}" \
					  --set-string telemetry.commit='not-a-full-git-sha' > /dev/null 2>&1; then
					  echo "chart must reject invalid release commit metadata" >&2
					  exit 1
					fi

					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  "${chart_secret_args[@]}" \
					  --set-string image.digest="${placeholder_digest}" > /dev/null 2>&1; then
					  echo "chart must reject the all-zero image digest placeholder" >&2
					  exit 1
					fi

					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  "${chart_secret_args[@]}" \
					  --set extensions.enabled=true \
					  --set-string 'extensions.modules[0].name=placeholder-extension' \
					  --set-string 'extensions.modules[0].registry=ghcr.io/waddle-social/waddle/extensions/placeholder-extension' \
					  --set-string "extensions.modules[0].digest=${placeholder_digest}" \
					  --set-string 'extensions.modules[0].namespace=urn:waddle:placeholder-extension:1' > /dev/null 2>&1; then
					  echo "chart must reject all-zero extension module digest placeholders" >&2
					  exit 1
					fi

					yq -o=yaml '.spec.values' ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > "${gitops_values}"
					if grep -R "${placeholder_digest}" ../infrastructure/waddle.cloud/gitops/waddle-server; then
					  echo "checked-in GitOps must not contain all-zero digest placeholders" >&2
					  exit 1
					fi
					if grep -R "github-enricher" ../infrastructure/waddle.cloud/gitops/waddle-server; then
					  echo "checked-in GitOps must not ship github-enricher" >&2
					  exit 1
					fi
					for forbidden in "helm.sh/hook" "secret-bootstrap" "bootstrap-secrets" "bitnami/kubectl"; do
					  if grep -R "${forbidden}" charts/waddle-server ../infrastructure/waddle.cloud/gitops/waddle-server; then
					    echo "chart/GitOps must not contain removed bootstrap hook token: ${forbidden}" >&2
					    exit 1
					  fi
					  if grep -q "${forbidden}" "${chart_render}"; then
					    echo "rendered chart must not contain removed bootstrap hook token: ${forbidden}" >&2
					    exit 1
					  fi
					done
					cue vet . "${gitops_values}" -d '#CheckedInGitOpsValues'
					helm lint charts/waddle-server -f "${gitops_values}"
					helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  -f "${gitops_values}" > "${gitops_render}"
					for env_and_value in \
					  "DEPLOYMENT_ENVIRONMENT_NAME:production" \
					  "DEPLOYMENT_CLUSTER_NAME:waddle-cloud"; do
					  env_name="${env_and_value%%:*}"
					  env_value="${env_and_value#*:}"
					  yq -e "select(.kind == \"Deployment\") | .spec.template.spec.containers[] | select(.name == \"waddle-server\") | (.env // [])[] | select(.name == \"${env_name}\" and .value == \"${env_value}\")" "${gitops_render}" > /dev/null
					done
					for env_name in WADDLE_SESSION_KEY WADDLE_OCCUPANT_ID_SECRET; do
					  yq -e "select(.kind == \"Deployment\") | .spec.template.spec.containers[] | select(.name == \"waddle-server\") | (.env // [])[] | select(.name == \"${env_name}\" and .valueFrom.secretKeyRef.name == \"waddle-runtime-secrets\" and .valueFrom.secretKeyRef.optional == false)" "${gitops_render}" > /dev/null
					done
					kubectl kustomize ../infrastructure/waddle.cloud/gitops/waddle-server > "${gitops_kustomize}"
					if grep -q "${placeholder_digest}" "${gitops_kustomize}"; then
					  echo "rendered GitOps must not contain all-zero digest placeholders" >&2
					  exit 1
					fi
					if grep -q "github-enricher" "${gitops_kustomize}"; then
					  echo "rendered GitOps must not contain github-enricher" >&2
					  exit 1
					fi
					for forbidden in "helm.sh/hook" "secret-bootstrap" "bootstrap-secrets" "bitnami/kubectl"; do
					  if grep -q "${forbidden}" "${gitops_render}" || grep -q "${forbidden}" "${gitops_kustomize}"; then
					    echo "rendered GitOps must not contain removed bootstrap hook token: ${forbidden}" >&2
					    exit 1
					  fi
					done

					cue export . -e '#PublishedExtensionModules' --out yaml \
					  -t linkBoardDigest="${sample_digest}" \
					  -t aiChatbotDigest="${sample_digest}" \
					  -t decisionPollsDigest="${sample_digest}" \
					  -t githubDigest="${sample_digest}" \
					  -t stargateQuotesDigest="${sample_digest}" > "${modules_yaml}"
					cp "${gitops_values}" "${published_values}"
					MODULES_YAML="${modules_yaml}" SAMPLE_DIGEST="${sample_digest}" SAMPLE_GIT_SHA="${sample_git_sha}" yq -i '
					  .image.digest = strenv(SAMPLE_DIGEST) |
					  .telemetry.commit = strenv(SAMPLE_GIT_SHA) |
					  .extensions.enabled = true |
					  .extensions.modules = load(strenv(MODULES_YAML))
					' "${published_values}"
					cp -R ../infrastructure/waddle.cloud/gitops/waddle-server/. "${published_gitops_dir}/"
					SAMPLE_DIGEST="${sample_digest}" yq -i '.spec.ref = {"digest": strenv(SAMPLE_DIGEST)}' "${published_gitops_dir}/chart-ocirepository.yaml"
					kubectl kustomize "${published_gitops_dir}" > "${published_gitops_kustomize}"
					yq -e "select(.kind == \"OCIRepository\" and .metadata.name == \"waddle-server-chart\") | (.spec.ref.digest == \"${sample_digest}\" and (.spec.ref.tag == null))" "${published_gitops_kustomize}" >/dev/null
					yq -e 'select(.kind == "HelmRelease" and .metadata.name == "waddle-server") | (.spec.chartRef.kind == "OCIRepository" and .spec.chartRef.name == "waddle-server-chart" and (.spec.chart == null))' "${published_gitops_kustomize}" >/dev/null
					cue vet . "${published_values}" -d '#PublishedValues' \
					  -t serverImageDigest="${sample_digest}" \
					  -t linkBoardDigest="${sample_digest}" \
					  -t aiChatbotDigest="${sample_digest}" \
					  -t decisionPollsDigest="${sample_digest}" \
					  -t githubDigest="${sample_digest}" \
					  -t stargateQuotesDigest="${sample_digest}"
					helm lint charts/waddle-server -f "${published_values}"
					helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  -f "${published_values}" > "${published_render}"

					rendered_image="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | .image' "${published_render}")"
					case "${rendered_image}" in
					  ghcr.io/waddle-social/waddle@sha256:1111111111111111111111111111111111111111111111111111111111111111) ;;
					  *) echo "published GitOps render must pin the server image by digest, got: ${rendered_image}" >&2; exit 1 ;;
					esac
					rendered_git_sha="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | (.env // [])[] | select(.name == "WADDLE_GIT_SHA") | .value' "${published_render}")"
					case "${rendered_git_sha}" in
					  "${sample_git_sha}") ;;
					  *) echo "published GitOps render must set WADDLE_GIT_SHA, got: ${rendered_git_sha:-<missing>}" >&2; exit 1 ;;
					esac
				"""#]
			inputs: list.Concat([_chartInputs, _gitopsWaddleServerInputs, _deploymentInputs])
		}

		buildContainerImage: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					if [ "$(uname -s)" != "Linux" ]; then
					  echo "waddle-server-image-stream is linux-only; run this with a Linux Nix builder or in CI." >&2
					  exit 1
					fi
					image_stream="$(nix build --print-out-paths ../#waddle-server-image-stream)"
					"${image_stream}" | docker image load
					expected_commit="$(git rev-parse HEAD)"
					embedded_commit="$(docker run --rm ghcr.io/waddle-social/waddle:nix --build-commit)"
					if [ "${embedded_commit}" != "${expected_commit}" ]; then
					  echo "container build identity ${embedded_commit} does not match source ${expected_commit}" >&2
					  exit 1
					fi
				"""#]
			inputs: _nixInputs
			dependsOn: [tasks.buildCi]
		}

		publishContainerImage: schema.#Task & {
			command: "bash"
			env: {
				CI_GITHUB_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
				CI_GITHUB_ACTOR: schema.#EnvPassthrough & {name: "GITHUB_ACTOR"}
				CI_GITHUB_REF_TYPE: schema.#EnvPassthrough & {name: "GITHUB_REF_TYPE"}
				CI_GITHUB_REF_NAME: schema.#EnvPassthrough & {name: "GITHUB_REF_NAME"}
				CUENV_ARCH: "amd64"
			}
			args: ["scripts/publish-waddle-release.sh"]
			inputs: list.Concat([_nixInputs, _chartInputs, _gitopsWaddleServerInputs, _deploymentInputs, _releasePublicationInputs])
			outputs: ["target/digests/**"]
			dependsOn: [
				tasks.checkRootSyncDrift,
				tasks.checkCiDrift,
				tasks.checkSwitchableAlternativeProgram,
				tasks.fmt,
				tasks.clippy,
				tasks.test,
				tasks.checkXmppClientFfiBindings,
				tasks.renderDeployment,
				tasks.buildExtensionModules,
			]
		}

		flakehubPublished: schema.#Task & {
			command: "true"
			inputs: [
				"../flake.nix",
				"../flake.lock",
			]
		}

		build: schema.#Task & {
			command: "cargo"
			args: ["build", "--bin", "waddle-server"]
			inputs: _rustInputs
		}

		dev: schema.#Task & {
			command: "cargo"
			args: [
				"run",
				"--bin",
				"waddle-server",
			]
			dependsOn: [tasks.build]
		}

		reconcileServerWithSource: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					flux reconcile source oci waddle-server -n flux-system
					flux reconcile kustomization infra-waddle-server -n flux-system --with-source
					flux reconcile helmrelease waddle-server -n waddle --with-source
				"""#]
		}

		// XMPP compliance tasks — run as the xmppCompliance pipeline
		nixXmppUnitTests: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-xmpp-unit-tests"]
			inputs: _nixInputs
		}

		nixXmppServerTests: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-xmpp-server-tests"]
			inputs: list.Concat([_nixInputs, _chartInputs])
		}

		nixXmppXepIntegration: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-xmpp-xep-integration"]
			inputs: _nixInputs
		}

		xmppUnitTests: xRust.#Test & {
			args: ["test", "--package", "waddle-xmpp", "--lib", "--verbose"]
			inputs: _rustInputs
		}

		xmppServerTests: xRust.#Test & {
			args: ["test", "--package", "waddle-server", "--verbose"]
			inputs: _rustInputs
		}

		xmppXepIntegration: xRust.#Test & {
			args: ["test", "--package", "waddle-xmpp", "--tests", "--verbose"]
			inputs: _rustInputs
		}

	}

	env: {
		CARGO_TERM_COLOR: "always"
		RUST_BACKTRACE:   "1"
		environment: test: {
			WADDLE_CERTS_EPHEMERAL:             "true"
			WADDLE_TEST_FIXED_ACCOUNT_ENABLED:  "true"
			WADDLE_TEST_FIXED_ACCOUNT_PASSWORD: "cuenv-test-password"
			WADDLE_UPLOAD_DIR:                  "./uploads"
			WADDLE_SESSION_KEY:                 "cuenv-test-session-key"
			// XEP-0421 occupant-id HMAC key. Test/CI value only — production
			// must set its own via the Helm chart or secret manager.
			WADDLE_OCCUPANT_ID_SECRET: "cuenv-test-occupant-id-secret-32-bytes-long"
		}
	}

}
