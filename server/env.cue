package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	xRust "github.com/cuenv/cuenv/contrib/rust"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

let _rustInputs = [
	"Cargo.toml",
	"Cargo.lock",
	".config/nextest.toml",
	"rust-toolchain.toml",
	"crates/**",
	"extensions/**",
	"scripts/**",
	"wit/**",
]

let _nixInputs = [
	"../flake.nix",
	"../flake.lock",
	// The nix test derivations copy the Mimir reliability rules into the
	// build tree for the #1436 registration drift guard, so rules edits
	// must re-run the Rust test lanes.
	"../infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml",
	"Cargo.toml",
	"Cargo.lock",
	".config/nextest.toml",
	"rust-toolchain.toml",
	"crates/**",
	"extensions/**",
	"wit/**",
]

// cargo-nextest task template: xRust.#Test pins args[0] to "test", but the
// runner subcommand is "nextest run", so nextest tasks use schema.#Task.
let _nextestTask = schema.#Task & {
	command: "cargo"
	dir: from: "caller"
	inputs: _rustInputs
}

let _chartInputs = ["charts/waddle-server/**"]
let _gitopsWaddleServerInputs = [
	"../infrastructure/waddle.cloud/gitops/waddle-server/**",
	"../infrastructure/waddle.cloud/gitops/waddle-server-source.yaml",
	"../infrastructure/waddle.cloud/gitops/kustomization-infra-waddle-server.yaml",
]
let _deploymentInputs = ["deployment.cue"]

schema.#Project & {
	name: "waddle-server"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	let _t = tasks

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#Hestia,
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
					priority: 5
					dependsOn: ["checkout.tag", "hestia.setup"]
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
				_t.doctest,
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
			tasks: [_t.checkRootSyncDrift, _t.checkCiDrift, _t.checkSwitchableAlternativeProgram, _t.nixFmt, _t.nixClippy, _t.nixTest, _t.nixDoctest, _t.checkXmppClientFfiBindings, _t.renderDeployment, _t.nixBuildExtensionModules, _t.nixBuildCi]
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
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					cd ..
					projects="$(
					  cuenv info --json |
					    bun -e 'const info = JSON.parse(await Bun.stdin.text()); if (!Array.isArray(info.projects) || info.projects.length === 0) throw new Error("cuenv info returned no projects"); for (const project of info.projects) { if (typeof project.path !== "string" || project.path.length === 0 || project.path.includes("\n")) throw new Error("cuenv info returned an invalid project path"); console.log(project.path); }'
					)"
					overall_status=0
					while IFS= read -r project; do
					  if ! cuenv sync ci --check -p "${project}"; then
					    overall_status=1
					  fi
					done <<< "${projects}"
					exit "${overall_status}"
				"""#]
			inputs: [
				"**/env.cue",
				"deployment.cue",
				"../.github/workflows/hestia-cache-gc.yml",
				"../ci/contributors/nix.cue",
				"../.github/workflows/waddle-server-*.yml",
				"../cue.mod/module.cue",
			]
		}

		checkRootSyncDrift: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
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
					bun test ../tests/critical-journeys.test.ts
				"""#]
			inputs: [
				"../apps/apple/**/Tests/**",
				"../chat/tests/**",
				"../docs/evidence/**",
				"capabilities.toml",
				"crates/**/tests/**",
				"crates/waddle-server/src/server/routes/websocket/tests/**",
				"crates/waddle-server/tests/server_capability_manifest.rs",
				"../docs/product/critical-journeys.json",
				"../docs/product/gate-evidence.json",
				"../docs/product/performance-profile.json",
				"../docs/planning/switchable-alternative.md",
				"../tests/**",
				"../cuenv.lock",
				"../xeps/xep-*.xml",
				"env.cue",
			]
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

		nixDoctest: schema.#Task & {
			command: "nix"
			args: ["build", "--print-build-logs", "../#checks.x86_64-linux.waddle-server-doctest"]
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

		test: _nextestTask & {
			args: ["nextest", "run", "--workspace", "--all-targets", "--locked", "--profile", "ci"]
		}

		// nextest cannot run doctests; keep them verified via cargo test --doc.
		doctest: xRust.#Test & {
			args: ["test", "--doc", "--workspace", "--all-features", "--locked"]
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
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test, tasks.doctest]
		}

		buildRelease: xRust.#Build & {
			args: ["build", "--release", "--locked", "--package", "waddle-server"]
			inputs: _rustInputs
			outputs: ["target/release/waddle-server"]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test, tasks.doctest]
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
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test, tasks.doctest]
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
					chart_secret_args=(
					  --set-string secret.sessionKey=ci-session-key
					  --set-string secret.occupantIdSecret=ci-occupant-id-secret-32-bytes-long
					)

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
					  .containerExtraEnv = ((.containerExtraEnv // []) | map(select(.name != "WADDLE_GIT_SHA"))) + [{"name": "WADDLE_GIT_SHA", "value": strenv(SAMPLE_GIT_SHA)}] |
					  .extensions.enabled = true |
					  .extensions.modules = load(strenv(MODULES_YAML))
					' "${published_values}"
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
			args: ["-c", #"""
					set -euo pipefail
					placeholder_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"
					GITHUB_TOKEN="${CI_GITHUB_TOKEN:?missing GITHUB_TOKEN}"
					GITHUB_ACTOR="${CI_GITHUB_ACTOR:?missing GITHUB_ACTOR}"
					GITHUB_REF_TYPE="${CI_GITHUB_REF_TYPE:?missing GITHUB_REF_TYPE}"
					GITHUB_REF_NAME="${CI_GITHUB_REF_NAME:?missing GITHUB_REF_NAME}"
					case "${CUENV_ARCH}" in
					  amd64) ;;
					  *) echo "unsupported CUENV_ARCH=${CUENV_ARCH}" >&2; exit 1 ;;
					esac

					echo "${GITHUB_TOKEN}" | docker login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
					FULL_SHA="$(git rev-parse HEAD)"
					SHORT_SHA="$(git rev-parse --short HEAD)"
					mkdir -p ../target/digests

					image_stream="$(nix build --print-out-paths ../#waddle-server-image-stream)"
					"${image_stream}" | docker image load

					docker tag ghcr.io/waddle-social/waddle:nix "ghcr.io/waddle-social/waddle:sha-${SHORT_SHA}"
					docker push "ghcr.io/waddle-social/waddle:sha-${SHORT_SHA}" 2>&1 | tee "../target/digests/push-${CUENV_ARCH}.log"
					digest="$(docker inspect --format='{{index .RepoDigests 0}}' "ghcr.io/waddle-social/waddle:sha-${SHORT_SHA}" | sed 's/^.*@//')"
					if [ -z "${digest}" ]; then
					  digest="$(grep -Eo 'sha256:[a-f0-9]{64}' "../target/digests/push-${CUENV_ARCH}.log" | tail -n1)"
					fi
					if [ -z "${digest}" ]; then
					  echo "No image digest found" >&2
					  exit 1
					fi
					if ! [[ "${digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
					  echo "Invalid image digest: ${digest}" >&2
					  exit 1
					fi
					if [ "${digest}" = "${placeholder_digest}" ]; then
					  echo "Refusing to publish GitOps with the all-zero image digest placeholder" >&2
					  exit 1
					fi
					printf '%s\n' "${digest}" > "../target/digests/${CUENV_ARCH}.txt"

					TAG_ARGS=()
					TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle:sha-${SHORT_SHA}")
					if [ "${GITHUB_REF_TYPE:-}" = "tag" ]; then
					  VERSION="${GITHUB_REF_NAME}"
					  TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle:${VERSION}")
					  if [[ "${VERSION}" =~ ^v?([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
					    MAJOR="${BASH_REMATCH[1]}"
					    MINOR="${BASH_REMATCH[2]}"
					    PATCH="${BASH_REMATCH[3]}"
					    TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle:${MAJOR}.${MINOR}.${PATCH}")
					    TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle:${MAJOR}.${MINOR}")
					    TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle:${MAJOR}")
					  fi
					else
					  TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle:main")
					fi

					docker buildx imagetools create "${TAG_ARGS[@]}" "ghcr.io/waddle-social/waddle@${digest}"
					docker buildx imagetools inspect "${TAG_ARGS[1]}"

					echo "${GITHUB_TOKEN}" | helm registry login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
					chart_package="$(helm package charts/waddle-server -d /tmp/charts | awk '{print $NF}')"
					chart_version="$(helm show chart "${chart_package}" | awk '$1 == "version:" {print $2; exit}')"
					helm_push_log="$(mktemp)"
					if ! helm push "${chart_package}" oci://ghcr.io/waddle-social/waddle/charts 2>&1 | tee "${helm_push_log}"; then
					  if grep -Eiq 'already exists|409|conflict' "${helm_push_log}"; then
					    remote_chart_dir="$(mktemp -d)"
					    local_chart_dir="$(mktemp -d)"
					    if helm pull oci://ghcr.io/waddle-social/waddle/charts/waddle-server --version "${chart_version}" --destination "${remote_chart_dir}" >/dev/null 2>&1; then
					      tar -xzf "${chart_package}" -C "${local_chart_dir}"
					      tar -xzf "${remote_chart_dir}/waddle-server-${chart_version}.tgz" -C "${remote_chart_dir}"
					      if diff -qr "${local_chart_dir}/waddle-server" "${remote_chart_dir}/waddle-server" >/dev/null; then
					        echo "Chart version ${chart_version} already exists with identical content, skipping"
					      else
					        echo "Chart version ${chart_version} already exists with different content; bump charts/waddle-server/Chart.yaml version" >&2
					        exit 1
					      fi
					    else
					      echo "Chart version ${chart_version} already exists but could not pull remote chart for comparison" >&2
					      exit 1
					    fi
					  else
					    exit 1
					  fi
					fi
					echo "${GITHUB_TOKEN}" | oras login ghcr.io -u "${GITHUB_ACTOR}" --password-stdin

					rustup target add wasm32-wasip2 >/dev/null 2>&1 || true
					declare -a EXTENSIONS=(
					  "link-board:link_board:urn:waddle:link-board:1"
					  "ai-chatbot:ai_chatbot:urn:waddle:ai-chatbot:1"
					  "decision-polls:decision_polls:urn:waddle:decision-polls:1"
					  "github:github:urn:waddle:web-integration:1"
					  "stargate-quotes:stargate_quotes:urn:waddle:stargate-quotes:1"
					)

					link_board_digest=""
					ai_chatbot_digest=""
					decision_polls_digest=""
					github_digest=""
					stargate_quotes_digest=""
					modules_yaml="../target/digests/extensions-modules.yaml"
					for extension_spec in "${EXTENSIONS[@]}"; do
					  IFS=: read -r extension_name crate_name _namespace_scheme _namespace_rest <<< "${extension_spec}"
					  wasm_path="target/wasm32-wasip2/release/${crate_name}.wasm"
					  extension_ref="ghcr.io/waddle-social/waddle/extensions/${extension_name}:sha-${SHORT_SHA}"

					  cargo build --release --locked --target wasm32-wasip2 --target-dir target --manifest-path "extensions/${extension_name}/Cargo.toml"
					  test -s "${wasm_path}"
					  if [ "${extension_name}" = "ai-chatbot" ]; then
					    if grep -aE "AI provider unavailable|WADDLE_AI_PROVIDER|OPENROUTER_API_KEY|OPENAI_API_KEY" "${wasm_path}" >/dev/null; then
					      echo "Refusing to publish ai-chatbot WASM with legacy server-provider fallback strings" >&2
					      exit 1
					    fi
					  fi
					  oras push \
					    --artifact-type "application/vnd.waddle.extension.wasm.v1+wasm" \
					    "${extension_ref}" \
					    "${wasm_path}:application/wasm"
					  extension_digest="$(oras resolve "${extension_ref}")"
					  if ! [[ "${extension_digest}" =~ ^sha256:[a-f0-9]{64}$ ]]; then
					    echo "Invalid digest for ${extension_name}: ${extension_digest}" >&2
					    exit 1
					  fi
					  if [ "${extension_digest}" = "${placeholder_digest}" ]; then
					    echo "Refusing to pin all-zero digest placeholder for ${extension_name}" >&2
					    exit 1
					  fi
					  case "${extension_name}" in
					    link-board) link_board_digest="${extension_digest}" ;;
					    ai-chatbot) ai_chatbot_digest="${extension_digest}" ;;
					    decision-polls) decision_polls_digest="${extension_digest}" ;;
					    github) github_digest="${extension_digest}" ;;
					    stargate-quotes) stargate_quotes_digest="${extension_digest}" ;;
					    *) echo "unknown extension ${extension_name}" >&2; exit 1 ;;
					  esac
					done
					cue export . -e '#PublishedExtensionModules' --out yaml \
					  -t linkBoardDigest="${link_board_digest:?missing link-board digest}" \
					  -t aiChatbotDigest="${ai_chatbot_digest:?missing ai-chatbot digest}" \
					  -t decisionPollsDigest="${decision_polls_digest:?missing decision-polls digest}" \
					  -t githubDigest="${github_digest:?missing github digest}" \
					  -t stargateQuotesDigest="${stargate_quotes_digest:?missing stargate-quotes digest}" > "${modules_yaml}"

					FULL_SHA="${FULL_SHA}" yq -i ".spec.values.image.tag = \"sha-${SHORT_SHA}\" | .spec.values.image.digest = \"${digest}\" | .spec.values.containerExtraEnv = ((.spec.values.containerExtraEnv // []) | map(select(.name != \"WADDLE_GIT_SHA\"))) + [{\"name\": \"WADDLE_GIT_SHA\", \"value\": strenv(FULL_SHA)}] | .spec.values.extensions.enabled = true" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
					yq -e ".spec.values.image.tag == \"sha-${SHORT_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -e ".spec.values.image.digest == \"${digest}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -e ".spec.values.containerExtraEnv[] | select(.name == \"WADDLE_GIT_SHA\") | .value == \"${FULL_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -i ".spec.values.extensions.modules = load(\"${modules_yaml}\")" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
					yq -e ".spec.values.extensions.enabled == true" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					if grep -R "${placeholder_digest}" ../infrastructure/waddle.cloud/gitops/waddle-server; then
					  echo "refusing to publish GitOps with all-zero digest placeholders" >&2
					  exit 1
					fi
					if grep -R "github-enricher" ../infrastructure/waddle.cloud/gitops/waddle-server; then
					  echo "refusing to publish GitOps with github-enricher" >&2
					  exit 1
					fi
					gitops_values="$(mktemp).yaml"
					gitops_render="$(mktemp)"
					yq -o=yaml '.spec.values' ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > "${gitops_values}"
					cue vet . "${gitops_values}" -d '#PublishedValues' \
					  -t serverImageDigest="${digest}" \
					  -t linkBoardDigest="${link_board_digest}" \
					  -t aiChatbotDigest="${ai_chatbot_digest}" \
					  -t decisionPollsDigest="${decision_polls_digest}" \
					  -t githubDigest="${github_digest}" \
					  -t stargateQuotesDigest="${stargate_quotes_digest}"
					helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  -f "${gitops_values}" > "${gitops_render}"
					rendered_image="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | .image' "${gitops_render}")"
					case "${rendered_image}" in
					  ghcr.io/waddle-social/waddle@"${digest}") ;;
					  *) echo "published GitOps render must pin the server image digest ${digest}, got: ${rendered_image}" >&2; exit 1 ;;
					esac
					rendered_git_sha="$(yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[] | select(.name == "waddle-server") | (.env // [])[] | select(.name == "WADDLE_GIT_SHA") | .value' "${gitops_render}")"
					case "${rendered_git_sha}" in
					  "${FULL_SHA}") ;;
					  *) echo "published GitOps render must set WADDLE_GIT_SHA=${FULL_SHA}, got: ${rendered_git_sha:-<missing>}" >&2; exit 1 ;;
					esac
					flux push artifact oci://ghcr.io/waddle-social/waddle/gitops-waddle-server:latest \
					  --path=../infrastructure/waddle.cloud/gitops/waddle-server \
					  --source="$(git config --get remote.origin.url)" \
					  --revision="${SHORT_SHA}"
				"""#]
			inputs: list.Concat([_nixInputs, _chartInputs, _gitopsWaddleServerInputs, _deploymentInputs])
			outputs: ["target/digests/**"]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test, tasks.doctest, tasks.renderDeployment, tasks.buildExtensionModules]
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

		xmppUnitTests: _nextestTask & {
			args: ["nextest", "run", "--package", "waddle-xmpp", "--lib", "--profile", "ci"]
		}

		xmppServerTests: _nextestTask & {
			args: ["nextest", "run", "--package", "waddle-server", "--profile", "ci"]
		}

		xmppXepIntegration: _nextestTask & {
			args: ["nextest", "run", "--package", "waddle-xmpp", "--tests", "--profile", "ci"]
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
