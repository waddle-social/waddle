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
	"crates/**",
	"extensions/**",
	"wit/**",
]

let _nixInputs = [
	"../flake.nix",
	"../flake.lock",
	"Cargo.toml",
	"Cargo.lock",
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

schema.#Project & {
	name: "waddle-server"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	let _t = tasks

	ci: providers: ["github"]
	ci: contributors: [
		c.#FlakeHubCache,
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
					id:       "determinate-nix"
					label:    "Setup Determinate Nix"
					priority: 2
					dependsOn: ["checkout.tag"]
					provider: github: uses: "DeterminateSystems/determinate-nix-action@f4c468f2287f0f14a113841b41f2dc8957119519"
				},
				{
					id:       "push"
					label:    "Push to FlakeHub"
					priority: 3
					dependsOn: ["determinate-nix"]
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
		flakehubCache: flakeName: "waddle-social/waddle"
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
			derivePaths: false
			provider: github: {
				permissions: {
					"id-token":      "write"
					packages:        "write"
					"pull-requests": "none"
				}
			}
			tasks: [
				_t.fmt,
				_t.clippy,
				_t.test,
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
			// cuenv currently emits parent-directory inputs as server/../... in
			// GitHub path filters, which do not reliably match repo-root files.
			derivePaths: false
			provider: github: permissions: {
				"id-token":      "write"
				contents:        "read"
				checks:          "write"
				packages:        "read"
				"pull-requests": "none"
			}
			tasks: [_t.checkCiDrift, _t.nixFmt, _t.nixClippy, _t.nixTest, _t.renderDeployment, _t.nixBuildExtensionModules, _t.nixBuildCi]
		}
		xmppCompliance: {
			mode: "expanded"
			when: {
				branch: ["main"]
				defaultBranch: true
				pullRequest:   true
				manual:        true
			}
			// Keep this broad until cuenv canonicalizes ../ inputs in derived
			// GitHub path filters; these Nix checks depend on repo-root flake files.
			derivePaths: false
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
			inputs: ["**/env.cue"]
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
					for module in links-task-board pub-quiz ai-chatbot ai-assistant-canvas decision-polls; do
					  cargo build --release --locked --target wasm32-wasip2 --target-dir target --manifest-path "extensions/${module}/Cargo.toml"
					done
					test -s target/wasm32-wasip2/release/links_task_board.wasm
					test -s target/wasm32-wasip2/release/pub_quiz.wasm
					test -s target/wasm32-wasip2/release/ai_chatbot.wasm
					test -s target/wasm32-wasip2/release/ai_assistant_canvas.wasm
					test -s target/wasm32-wasip2/release/decision_polls.wasm
				"""#]
			inputs: _rustInputs
			outputs: [
				"server/target/wasm32-wasip2/release/links_task_board.wasm",
				"server/target/wasm32-wasip2/release/pub_quiz.wasm",
				"server/target/wasm32-wasip2/release/ai_chatbot.wasm",
				"server/target/wasm32-wasip2/release/ai_assistant_canvas.wasm",
				"server/target/wasm32-wasip2/release/decision_polls.wasm",
			]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test]
		}

		renderDeployment: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail

					placeholder_digest="sha256:0000000000000000000000000000000000000000000000000000000000000000"
					sample_digest="sha256:1111111111111111111111111111111111111111111111111111111111111111"
					chart_render="$(mktemp)"
					gitops_values="$(mktemp)"
					gitops_render="$(mktemp)"
					gitops_kustomize="$(mktemp)"
					published_values="$(mktemp)"
					published_render="$(mktemp)"

					helm lint charts/waddle-server --set spicedb.enabled=false
					helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false > "${chart_render}"

					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
					  --set-string image.digest="${placeholder_digest}" > /dev/null 2>&1; then
					  echo "chart must reject the all-zero image digest placeholder" >&2
					  exit 1
					fi

					if helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  --set spicedb.enabled=false \
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
					yq -e '.extensions.enabled == false' "${gitops_values}" > /dev/null
					yq -e '(.extensions.modules // []) | length == 0' "${gitops_values}" > /dev/null
					helm lint charts/waddle-server -f "${gitops_values}"
					helm template waddle-server charts/waddle-server \
					  --namespace waddle \
					  -f "${gitops_values}" > "${gitops_render}"
					kubectl kustomize ../infrastructure/waddle.cloud/gitops/waddle-server > "${gitops_kustomize}"
					if grep -q "${placeholder_digest}" "${gitops_kustomize}"; then
					  echo "rendered GitOps must not contain all-zero digest placeholders" >&2
					  exit 1
					fi
					if grep -q "github-enricher" "${gitops_kustomize}"; then
					  echo "rendered GitOps must not contain github-enricher" >&2
					  exit 1
					fi

					cp "${gitops_values}" "${published_values}"
					SAMPLE_DIGEST="${sample_digest}" yq -i '
					  .image.digest = strenv(SAMPLE_DIGEST) |
					  .containerExtraEnv = ((.containerExtraEnv // []) | map(select(.name != "WADDLE_GIT_SHA"))) + [{"name": "WADDLE_GIT_SHA", "value": "1111111111111111111111111111111111111111"}] |
					  .extensions.enabled = true |
					  .extensions.modules = [
					    {"name": "links-task-board", "registry": "ghcr.io/waddle-social/waddle/extensions/links-task-board", "digest": strenv(SAMPLE_DIGEST), "namespace": "urn:waddle:links-task-board:1", "config": {}, "capabilityGrants": ["message.enrich", "commands", "launch", "pubsub.publish", "artifact.reference", "ui.declarative"]},
					    {"name": "pub-quiz", "registry": "ghcr.io/waddle-social/waddle/extensions/pub-quiz", "digest": strenv(SAMPLE_DIGEST), "namespace": "urn:waddle:pub-quiz:1", "config": {}, "capabilityGrants": ["message.enrich", "commands", "launch", "pubsub.publish", "ui.declarative"]},
					    {"name": "ai-chatbot", "registry": "ghcr.io/waddle-social/waddle/extensions/ai-chatbot", "digest": strenv(SAMPLE_DIGEST), "namespace": "urn:waddle:ai-chatbot:1", "config": {"endpoint": "https://openrouter.ai/api/v1/chat/completions", "model": "openrouter/auto"}, "configSecretFiles": {"api_key": "/var/run/secrets/waddle-ai/api_key"}, "capabilityGrants": ["message.enrich", "message.observe", "host.mam.read", "host.members.read", "host.presence.read", "host.roster.read", "host.channels.read", "host.spaces.read", "host.message.send", "outbound.http.request", "commands"], "allowedHttpOrigins": ["https://openrouter.ai"]},
					    {"name": "ai-assistant-canvas", "registry": "ghcr.io/waddle-social/waddle/extensions/ai-assistant-canvas", "digest": strenv(SAMPLE_DIGEST), "namespace": "urn:waddle:ai-assistant-canvas:1", "config": {}, "capabilityGrants": ["message.enrich", "commands", "launch", "pubsub.publish", "artifact.reference", "ui.declarative"]},
					    {"name": "decision-polls", "registry": "ghcr.io/waddle-social/waddle/extensions/decision-polls", "digest": strenv(SAMPLE_DIGEST), "namespace": "urn:waddle:decision-polls:1", "config": {}, "capabilityGrants": ["message.enrich", "commands", "launch", "pubsub.publish", "ui.declarative"]}
					  ]
					' "${published_values}"
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
					  1111111111111111111111111111111111111111) ;;
					  *) echo "published GitOps render must set WADDLE_GIT_SHA, got: ${rendered_git_sha:-<missing>}" >&2; exit 1 ;;
					esac

					yq -e '.extensions.enabled == true' "${published_values}" > /dev/null
					yq -e '.extensions.modules | length == 5' "${published_values}" > /dev/null

					expected_modules="ai-assistant-canvas ai-chatbot decision-polls links-task-board pub-quiz"
					actual_modules="$(yq -r '.extensions.modules[].name' "${published_values}" | sort | tr '\n' ' ' | sed 's/ $//')"
					if [ "${actual_modules}" != "${expected_modules}" ]; then
					  echo "GitOps extensions.modules must contain exactly: ${expected_modules}; got: ${actual_modules}" >&2
					  exit 1
					fi

					for module in links-task-board pub-quiz ai-chatbot ai-assistant-canvas decision-polls; do
					  yq -e ".extensions.modules[] | select(.name == \"${module}\") | .registry == \"ghcr.io/waddle-social/waddle/extensions/${module}\"" "${published_values}" > /dev/null
					  yq -e ".extensions.modules[] | select(.name == \"${module}\") | .digest == \"${sample_digest}\"" "${published_values}" > /dev/null
					done
				"""#]
			inputs: list.Concat([_chartInputs, _gitopsWaddleServerInputs])
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
					  "links-task-board:links_task_board:urn:waddle:links-task-board:1"
					  "pub-quiz:pub_quiz:urn:waddle:pub-quiz:1"
					  "ai-chatbot:ai_chatbot:urn:waddle:ai-chatbot:1"
					  "ai-assistant-canvas:ai_assistant_canvas:urn:waddle:ai-assistant-canvas:1"
					  "decision-polls:decision_polls:urn:waddle:decision-polls:1"
					)

					modules_yaml="../target/digests/extensions-modules.yaml"
					: > "${modules_yaml}"
					for extension_spec in "${EXTENSIONS[@]}"; do
					  IFS=: read -r extension_name crate_name namespace_scheme namespace_rest <<< "${extension_spec}"
					  namespace="${namespace_scheme}:${namespace_rest}"
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
					  {
					    printf -- '- name: %s\n' "${extension_name}"
					    printf '  registry: ghcr.io/waddle-social/waddle/extensions/%s\n' "${extension_name}"
					    printf '  digest: %s\n' "${extension_digest}"
					    printf '  namespace: %s\n' "${namespace}"
					    if [ "${extension_name}" = "ai-chatbot" ]; then
					      printf '  config:\n'
					      printf '    endpoint: https://openrouter.ai/api/v1/chat/completions\n'
					      printf '    model: openrouter/free\n'
					      printf '  configSecretFiles:\n'
					      printf '    api_key: /var/run/secrets/waddle-ai/api_key\n'
					      printf '  capabilityGrants:\n'
					      printf '    - message.enrich\n'
					      printf '    - message.observe\n'
					      printf '    - host.mam.read\n'
					      printf '    - host.members.read\n'
					      printf '    - host.presence.read\n'
					      printf '    - host.roster.read\n'
					      printf '    - host.channels.read\n'
					      printf '    - host.spaces.read\n'
					      printf '    - host.message.send\n'
					      printf '    - outbound.http.request\n'
					      printf '    - commands\n'
					      printf '  allowedHttpOrigins:\n'
					      printf '    - https://openrouter.ai\n'
					    else
					      printf '  config: {}\n'
					      printf '  capabilityGrants:\n'
					      printf '    - message.enrich\n'
					      printf '    - commands\n'
					      printf '    - launch\n'
					      printf '    - pubsub.publish\n'
					      printf '    - ui.declarative\n'
					    fi
					    if [ "${extension_name}" = "links-task-board" ] || [ "${extension_name}" = "ai-assistant-canvas" ]; then
					      printf '    - artifact.reference\n'
					    fi
					  } >> "${modules_yaml}"
					done

					FULL_SHA="${FULL_SHA}" yq -i ".spec.values.image.tag = \"sha-${SHORT_SHA}\" | .spec.values.image.digest = \"${digest}\" | .spec.values.containerExtraEnv = ((.spec.values.containerExtraEnv // []) | map(select(.name != \"WADDLE_GIT_SHA\"))) + [{\"name\": \"WADDLE_GIT_SHA\", \"value\": strenv(FULL_SHA)}] | .spec.values.extensions.enabled = true" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
					yq -e ".spec.values.image.tag == \"sha-${SHORT_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -e ".spec.values.image.digest == \"${digest}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -e ".spec.values.containerExtraEnv[] | select(.name == \"WADDLE_GIT_SHA\") | .value == \"${FULL_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -i ".spec.values.extensions.modules = load(\"${modules_yaml}\")" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
					yq -e ".spec.values.extensions.enabled == true" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					yq -e ".spec.values.extensions.modules | length == 5" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
					if grep -R "${placeholder_digest}" ../infrastructure/waddle.cloud/gitops/waddle-server; then
					  echo "refusing to publish GitOps with all-zero digest placeholders" >&2
					  exit 1
					fi
					if grep -R "github-enricher" ../infrastructure/waddle.cloud/gitops/waddle-server; then
					  echo "refusing to publish GitOps with github-enricher" >&2
					  exit 1
					fi
					gitops_values="$(mktemp)"
					gitops_render="$(mktemp)"
					yq -o=yaml '.spec.values' ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > "${gitops_values}"
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
			inputs: list.Concat([_nixInputs, _chartInputs, _gitopsWaddleServerInputs])
			outputs: ["target/digests/**"]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test, tasks.renderDeployment, tasks.buildExtensionModules]
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
			inputs: _nixInputs
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
			// XEP-0421 occupant-id HMAC key. Test/CI value only — production
			// must set its own via the Helm chart or secret manager.
			WADDLE_OCCUPANT_ID_SECRET: "cuenv-test-occupant-id-secret-32-bytes-long"
		}
	}

}
