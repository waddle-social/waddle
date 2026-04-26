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
	"wit/**",
]

let _nixInputs = [
	"../flake.nix",
	"../flake.lock",
	"Cargo.toml",
	"Cargo.lock",
	"crates/**",
	"wit/**",
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
		schema.#Contributor & {
			id: "nix"
			tasks: [{
				id:       "nix.install"
				label:    "Install Nix"
				priority: 0
				provider: github: uses: "DeterminateSystems/determinate-nix-action@v3"
			}]
		},
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
						uses: "actions/checkout@v6"
						with: {
							"persist-credentials": false
							ref: "${{ (inputs.tag != null) && format('refs/tags/{0}', inputs.tag) || '' }}"
						}
					}
				},
				{
					id:        "determinate-nix"
					label:     "Setup Determinate Nix"
					priority: 2
					dependsOn: ["checkout.tag"]
					provider: github: uses: "DeterminateSystems/determinate-nix-action@v3"
				},
				{
					id:        "push"
					label:     "Push to FlakeHub"
					priority: 3
					dependsOn: ["determinate-nix"]
					provider: github: {
						uses: "DeterminateSystems/flakehub-push@main"
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

	ci: pipelines: {
		default: {
			mode: "expanded"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: {
				permissions: packages: "write"
				runners: arch: {
					amd64: "ubuntu-latest"
				}
			}
			tasks: [
				_t.fmt,
				_t.clippy,
				_t.test,
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
				"id-token": "write"
				contents:   "read"
			}
			tasks: [_t.flakehubPublished]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			tasks: [_t.checkCiDrift, _t.fmt, _t.clippy, _t.test, _t.buildCi]
		}
		xmppCompliance: {
			when: {
				branch: ["main"]
				defaultBranch: true
				pullRequest:   true
				manual:        true
			}
			tasks: [
				_t.xmppUnitTests,
				_t.xmppServerTests,
				_t.xmppXepIntegration,
			]
		}
		githubEnricher: {
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			derivePaths: true
			tasks: [_t.buildGithubEnricher, _t.pushGithubEnricherOci, _t.pinGithubEnricherTag, _t.pushGithubEnricherGitops]
		}
		githubEnricherPullRequest: {
			when: {
				pullRequest: true
			}
			derivePaths: true
			tasks: [_t.buildGithubEnricher]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			command: "cuenv"
			args: ["sync", "ci", "--check", "-A"]
			inputs: ["**/env.cue"]
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
				GITHUB_TOKEN:    schema.#EnvPassthrough
				GITHUB_ACTOR:    "${{ github.actor }}"
				GITHUB_REF_TYPE: "${{ github.ref_type }}"
				GITHUB_REF_NAME: "${{ github.ref_name }}"
				CUENV_ARCH:      "amd64"
			}
			args: ["-c", #"""
				set -euo pipefail
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
				helm package charts/waddle-server -d /tmp/charts
				helm push /tmp/charts/waddle-server-*.tgz oci://ghcr.io/waddle-social/waddle/charts || echo "Chart version already exists, skipping"

				yq -i ".spec.values.image.tag = \"sha-${SHORT_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
				yq -e ".spec.values.image.tag == \"sha-${SHORT_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
				flux push artifact oci://ghcr.io/waddle-social/waddle/gitops-waddle-server:latest \
				  --path=../infrastructure/waddle.cloud/gitops/waddle-server \
				  --source="$(git config --get remote.origin.url)" \
				  --revision="${SHORT_SHA}"
				"""#]
			inputs: list.Concat([_nixInputs, ["charts/**"]])
			outputs: ["target/digests/**"]
			dependsOn: [tasks.fmt, tasks.clippy, tasks.test]
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

		buildGithubEnricher: schema.#Task & {
			command: "cargo"
			args: [
				"build",
				"--release",
				"--target", "wasm32-wasip2",
				"--target-dir", "target",
				"--manifest-path", "extensions/github-enricher/Cargo.toml",
			]
			inputs: ["extensions/github-enricher/**", "Cargo.lock"]
			outputs: ["target/wasm32-wasip2/release/github_enricher.wasm"]
		}

		pushGithubEnricherOci: schema.#Task & {
			command: "bash"
			env: {
				GITHUB_TOKEN: schema.#EnvPassthrough
				GITHUB_ACTOR: schema.#EnvPassthrough
			}
			args: ["-c", #"""
					set -euo pipefail
					WASM_FILE="target/wasm32-wasip2/release/github_enricher.wasm"
					IMAGE="ghcr.io/waddle-social/waddle/extensions/github-enricher"
					echo "${GITHUB_TOKEN}" | oras login ghcr.io -u "${GITHUB_ACTOR}" --password-stdin
					FULL_SHA="$(git rev-parse HEAD)"
					oras push "${IMAGE}:sha-${FULL_SHA}" \
					  --artifact-type application/vnd.waddle.extension.wasm.v1+wasm \
					  "${WASM_FILE}:application/wasm"
					oras push "${IMAGE}:main" \
					  --artifact-type application/vnd.waddle.extension.wasm.v1+wasm \
					  "${WASM_FILE}:application/wasm"
					oras push "${IMAGE}:latest" \
					  --artifact-type application/vnd.waddle.extension.wasm.v1+wasm \
					  "${WASM_FILE}:application/wasm"
				"""#]
			dependsOn: [tasks.buildGithubEnricher]
		}

		pinGithubEnricherTag: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
					set -euo pipefail
					FULL_SHA="$(git rev-parse HEAD)"
					yq -i "(.spec.values.extensions.modules[] | select(.name == \"github-enricher\") | .tag) = \"sha-${FULL_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
					yq -e ".spec.values.extensions.modules[] | select(.name == \"github-enricher\") | .tag == \"sha-${FULL_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
				"""#]
			dependsOn: [tasks.pushGithubEnricherOci]
		}

		pushGithubEnricherGitops: schema.#Task & {
			command: "bash"
			env: {
				GITHUB_TOKEN: schema.#EnvPassthrough
				GITHUB_ACTOR: schema.#EnvPassthrough
			}
			args: ["-c", #"""
				set -euo pipefail
					echo "${GITHUB_TOKEN}" | docker login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
					flux push artifact \
					  oci://ghcr.io/waddle-social/waddle/gitops-waddle-server:latest \
					  --path=../infrastructure/waddle.cloud/gitops/waddle-server \
					  --source="$(git config --get remote.origin.url)" \
					  --revision="$(git rev-parse --short HEAD)"
				"""#]
			dependsOn: [tasks.pinGithubEnricherTag]
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
		}
	}

}
