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
		c.#Nix,
		c.#CuenvRelease,
		c.#OnePassword,
	]

	ci: pipelines: {
		default: {
			when: {
				branch:        ["main"]
				defaultBranch: true
				manual:        true
			}
			tasks: [_t.fmt, _t.clippy, _t.test, _t.buildRelease, _t.publishContainerImage, _t.helmPush, _t.gitopsPush]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			tasks: [_t.checkCiDrift, _t.fmt, _t.clippy, _t.test, _t.buildCi]
		}
		xmppCompliance: {
			when: {
				branch:        ["main"]
				defaultBranch: true
				pullRequest:   true
				manual:        true
			}
			tasks: [
				_t.xmppUnitTests,
				_t.xmppServerTests,
				_t.xmppProtocolConformance,
				_t.xmppComplianceFastRegression,
				_t.xmppXepIntegration,
				_t.xmppE2eMessaging,
				_t.xmppFederatedMuc,
				_t.xmppS2sIntegration,
			]
		}
		githubEnricher: {
			when: {
				branch:        ["main"]
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

	images: {
		server: schema.#ContainerImage & {
			context:    "."
			dockerfile: "Containerfile"
			registry:   "ghcr.io/waddle-social/waddle"
			platform: ["linux/amd64", "linux/arm64"]
			tags: ["main"]
			inputs: list.Concat([_rustInputs, ["Containerfile", ".dockerignore"]])
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			command: "cuenv"
			args: ["sync", "ci", "--check", "-A"]
			inputs: ["**/env.cue"]
		}

		fmt: xRust.#Fmt & {
			args:   ["fmt", "--all", "--", "--check"]
			inputs: _rustInputs
		}

		clippy: xRust.#Clippy & {
			args:   ["clippy", "--all-targets", "--all-features", "--", "-D", "clippy::correctness"]
			inputs: _rustInputs
		}

		test: xRust.#Test & {
			args:   ["test", "--workspace", "--all-targets", "--locked"]
			inputs: _rustInputs
		}

		buildCi: xRust.#Build & {
			args:    ["build", "--profile", "ci", "--locked", "--package", "waddle-server"]
			inputs:  _rustInputs
			outputs: ["target/ci/waddle-server"]
		}

		buildRelease: xRust.#Build & {
			args:    ["build", "--release", "--locked", "--package", "waddle-server"]
			inputs:  _rustInputs
			outputs: ["target/release/waddle-server"]
		}

		publishContainerImage: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
				set -euo pipefail
				echo "${GITHUB_TOKEN}" | docker login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
				docker buildx create --use --name multiarch || true
				FULL_SHA="$(git rev-parse HEAD)"
				SHORT_SHA="$(git rev-parse --short HEAD)"
				docker buildx build \
				  --platform linux/amd64,linux/arm64 \
				  --build-arg WADDLE_GIT_SHA="${FULL_SHA}" \
				  --tag ghcr.io/waddle-social/waddle:sha-${SHORT_SHA} \
				  --tag ghcr.io/waddle-social/waddle:main \
				  --file Containerfile \
				  --push \
				  .
			"""#]
			inputs: list.Concat([_rustInputs, ["Containerfile", ".dockerignore"]])
		}

		helmPush: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
				echo "${GITHUB_TOKEN}" | helm registry login ghcr.io --username "${GITHUB_ACTOR}" --password-stdin
				helm package charts/waddle-server -d /tmp/charts
				helm push /tmp/charts/waddle-server-*.tgz oci://ghcr.io/waddle-social/waddle/charts || echo "Chart version already exists, skipping"
			"""#]
			inputs: ["charts/**"]
		}

		gitopsPush: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
				SHORT_SHA=$(git rev-parse --short HEAD)
				sed -i "s/tag: main/tag: sha-${SHORT_SHA}/" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
				flux push artifact oci://ghcr.io/waddle-social/waddle/gitops:latest \
				  --path=../infrastructure/waddle.cloud/gitops \
				  --source="$(git config --get remote.origin.url)" \
				  --revision="${SHORT_SHA}"
			"""#]
			inputs: ["../infrastructure/waddle.cloud/gitops/**"]
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

		// XMPP compliance tasks — run as the xmppCompliance pipeline
		xmppUnitTests: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--lib", "--verbose"]
			inputs: _rustInputs
		}

		xmppServerTests: xRust.#Test & {
			args:   ["test", "--package", "waddle-server", "--verbose"]
			inputs: _rustInputs
		}

		xmppProtocolConformance: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--test", "protocol_conformance", "--verbose"]
			inputs: _rustInputs
		}

		xmppComplianceFastRegression: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--test", "compliance_fast_regression", "--verbose"]
			inputs: _rustInputs
		}

		xmppXepIntegration: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--tests", "--verbose"]
			inputs: _rustInputs
		}

		xmppE2eMessaging: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--test", "messaging_e2e", "--verbose"]
			inputs: _rustInputs
		}

		xmppFederatedMuc: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--test", "federated_muc", "--verbose"]
			inputs: _rustInputs
		}

		xmppS2sIntegration: xRust.#Test & {
			args:   ["test", "--package", "waddle-xmpp", "--test", "s2s_integration", "--verbose"]
			inputs: _rustInputs
		}

		buildGithubEnricher: schema.#Task & {
			command: "cargo"
			args: [
				"build",
				"--release",
				"--target", "wasm32-wasip2",
				"--manifest-path", "extensions/github-enricher/Cargo.toml",
			]
			inputs:  ["extensions/github-enricher/**", "Cargo.lock"]
			outputs: ["target/wasm32-wasip2/release/github_enricher.wasm"]
		}

		pushGithubEnricherOci: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
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
		}

		pinGithubEnricherTag: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
				FULL_SHA="$(git rev-parse HEAD)"
				yq -i ".spec.values.extensions.modules[] |= (if .name == \"github-enricher\" then .tag = \"sha-${FULL_SHA}\" else . end)" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml
				yq -e ".spec.values.extensions.modules[] | select(.name == \"github-enricher\") | .tag == \"sha-${FULL_SHA}\"" ../infrastructure/waddle.cloud/gitops/waddle-server/helmrelease.yaml > /dev/null
			"""#]
		}

		pushGithubEnricherGitops: schema.#Task & {
			command: "bash"
			args: ["-c", #"""
				flux push artifact \
				  oci://ghcr.io/waddle-social/waddle/gitops:latest \
				  --path=../infrastructure/waddle.cloud/gitops \
				  --source="$(git config --get remote.origin.url)" \
				  --revision="$(git rev-parse --short HEAD)"
			"""#]
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
