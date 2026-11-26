package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

let _rustInputs = [
	"Cargo.toml",
	"Cargo.lock",
	"rust-toolchain.toml",
	"crates/**",
	"scripts/**",
	schema.#ModuleFiles & {paths: [
		"ci/cache/nix-shell-hook.sh",
		"flake.nix",
		"flake.lock",
	]},
]

let _crdDrift = {
	command: "bash"
	args: ["-euo", "pipefail", "-c", #"""
		waddle-operator-crd --kind server | diff -u config/crd/waddleservers.waddle.social.yaml -
		waddle-operator-crd --kind tenant | diff -u config/crd/waddletenants.waddle.social.yaml -
		diff -u config/crd/waddleservers.waddle.social.yaml ../infrastructure/waddle.cloud/gitops/waddle-operator/crds/waddleservers.waddle.social.yaml
		diff -u config/crd/waddletenants.waddle.social.yaml ../infrastructure/waddle.cloud/gitops/waddle-operator/crds/waddletenants.waddle.social.yaml
		"""#]
	inputs: [
		for input in _rustInputs {input},
		"config/crd/waddleservers.waddle.social.yaml",
		"config/crd/waddletenants.waddle.social.yaml",
		schema.#ModuleFiles & {paths: [
			"infrastructure/waddle.cloud/gitops/waddle-operator/crds/waddleservers.waddle.social.yaml",
			"infrastructure/waddle.cloud/gitops/waddle-operator/crds/waddletenants.waddle.social.yaml",
		]},
	]
}

schema.#Project & {
	name: "waddle-operator"

	runtime: {
		type:  "nix"
		flake: ".."
		output: "operator"
	}

	cache: remote: wc.#NamespaceRemoteCache
	contexts: nixSandbox: schema.#TaskFilter & {name: "ciCheckCrdDrift"}
	env: context: nixSandbox: wc.#NixSandboxScratch

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#FlakeHubCache,
		wc.#Hestia,
		wc.#CuenvRelease,
		wc.#NamespaceBazelCache,
	]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64": "namespace-profile-linux-x86"
			amd64:       "namespace-profile-linux-x86"
		}
	}

	ci: pipelines: {
		// The cache benchmark runs this task on two separate runners.
		cacheVerification: {
			when: manual: true
			"tasks": [tasks.ciCheckCrdDrift]
		}
		default: {
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: {
				// Privileged so the Nix build sandbox and Docker daemon work when
				// building and publishing the operator container image.
				runner: "namespace-profile-linux-x86;container.privileged=true"
				permissions: {contents: "read", "id-token": "write", packages: "write"}
			}
			"tasks": [
				tasks.fmt,
				tasks.structure,
				tasks.clippy,
				tasks.test,
				tasks.doctest,
				tasks.ciCheckCrdDrift,
				tasks.publishContainerImage,
			]
		}
		queueChecks: {
			when: manual: true
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [tasks.checkCiDrift, tasks.fmt, tasks.structure, tasks.ciCheckCrdDrift, tasks.clippy, tasks.test, tasks.doctest]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			hermetic: false
			command:  "cuenv"
			args: ["sync", "ci", "--check", "-p", "."]
			inputs: ["env.cue", schema.#ModuleFiles & {paths: [".github/workflows/waddle-operator-default.yml", ".github/workflows/waddle-operator-queuechecks.yml"]}]
		}

		fmt: schema.#Task & {
			command: "cargo"
			args: ["fmt", "--all", "--check"]
			inputs: _rustInputs
		}

		structure: schema.#Task & {
			command: "python3"
			args: ["scripts/check-rust-structure.py"]
			inputs: ["crates/**", "scripts/**"]
		}

		clippy: schema.#Task & {
			command: "cargo"
			args: ["clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]
			inputs: _rustInputs
		}

		test: schema.#Task & {
			command: "cargo"
			args: ["nextest", "run", "--workspace", "--all-targets", "--locked"]
			inputs: _rustInputs
		}

		doctest: schema.#Task & {
			command: "cargo"
			args: ["test", "--doc", "--workspace", "--locked"]
			inputs: _rustInputs
		}

		checkCrdDrift: schema.#Task & _crdDrift

		// Both Linux CI pipelines use the same confined action. Developers
		// can also check the CRD on hosts without Linux namespace support.
		ciCheckCrdDrift: schema.#Task & _crdDrift & {
			cache: mode:       "read-write"
			hermetic: sandbox: "strict"
		}

		publishContainerImage: schema.#Task & {
			ci: github: permissions: packages: "write"
			hermetic: false
			command:  "bash"
			env: {
				CI_GITHUB_TOKEN:    schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
				CI_GITHUB_ACTOR:    schema.#EnvPassthrough & {name: "GITHUB_ACTOR"}
				CI_GITHUB_REF_TYPE: schema.#EnvPassthrough & {name: "GITHUB_REF_TYPE"}
				CI_GITHUB_REF_NAME: schema.#EnvPassthrough & {name: "GITHUB_REF_NAME"}
				CUENV_ARCH:         "amd64"
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
					SHORT_SHA="$(git rev-parse --short HEAD)"
					mkdir -p ../target/operator-digests

					image_stream="$(nix build --print-out-paths --no-link ../#waddle-operator-image-stream)"
					"${image_stream}" | docker image load

					docker tag ghcr.io/waddle-social/waddle-operator:nix "ghcr.io/waddle-social/waddle-operator:sha-${SHORT_SHA}"
					docker push "ghcr.io/waddle-social/waddle-operator:sha-${SHORT_SHA}" 2>&1 | tee "../target/operator-digests/push-${CUENV_ARCH}.log"
					digest="$(docker inspect --format='{{index .RepoDigests 0}}' "ghcr.io/waddle-social/waddle-operator:sha-${SHORT_SHA}" | sed 's/^.*@//')"
					if [ -z "${digest}" ]; then
					  digest="$(grep -Eo 'sha256:[a-f0-9]{64}' "../target/operator-digests/push-${CUENV_ARCH}.log" | tail -n1)"
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
					  echo "Refusing to publish operator image with the all-zero image digest placeholder" >&2
					  exit 1
					fi
					printf '%s\n' "${digest}" > "../target/operator-digests/${CUENV_ARCH}.txt"

					TAG_ARGS=()
					TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle-operator:sha-${SHORT_SHA}")
					if [ "${GITHUB_REF_TYPE:-}" = "tag" ]; then
					  VERSION="${GITHUB_REF_NAME}"
					  TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle-operator:${VERSION}")
					  if [[ "${VERSION}" =~ ^v?([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
					    MAJOR="${BASH_REMATCH[1]}"
					    MINOR="${BASH_REMATCH[2]}"
					    PATCH="${BASH_REMATCH[3]}"
					    TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle-operator:${MAJOR}.${MINOR}.${PATCH}")
					    TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle-operator:${MAJOR}.${MINOR}")
					    TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle-operator:${MAJOR}")
					  fi
					else
					  TAG_ARGS+=("-t" "ghcr.io/waddle-social/waddle-operator:main")
					fi

					docker buildx imagetools create "${TAG_ARGS[@]}" "ghcr.io/waddle-social/waddle-operator@${digest}"
					docker buildx imagetools inspect "${TAG_ARGS[1]}"
				"""#]
			inputs: _rustInputs
			dependsOn: [
				tasks.fmt,
				tasks.structure,
				tasks.clippy,
				tasks.test,
				tasks.doctest,
				tasks.ciCheckCrdDrift,
			]
		}
	}
}
