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
	args:    ["-euo", "pipefail", "-c", "waddle-operator-crd | diff -u config/crd/waddleservers.waddle.social.yaml -"]
	inputs:  [for input in _rustInputs {input}, "config/crd/waddleservers.waddle.social.yaml"]
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
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [
				tasks.fmt,
				tasks.structure,
				tasks.clippy,
				tasks.test,
				tasks.doctest,
				tasks.ciCheckCrdDrift,
			]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [
				tasks.checkCiDrift,
				tasks.fmt,
				tasks.structure,
				tasks.ciCheckCrdDrift,
			]
		}
		queueChecks: {
			when: manual: true
			provider: github: permissions: {contents: "read", "id-token": "write"}
			"tasks": [tasks.clippy, tasks.test, tasks.doctest]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			hermetic: false
			command:  "cuenv"
			args: ["sync", "ci", "--check", "-p", "."]
			inputs: ["env.cue", schema.#ModuleFiles & {paths: [".github/workflows/waddle-operator-default.yml", ".github/workflows/waddle-operator-pullrequest.yml", ".github/workflows/waddle-operator-queuechecks.yml"]}]
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
	}
}
