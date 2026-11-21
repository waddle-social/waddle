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
		"flake.nix",
		"flake.lock",
	]},
]

schema.#Project & {
	name: "waddle-operator"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#FlakeHubCache,
		wc.#Hestia,
		wc.#CuenvRelease,
	]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64": "namespace-profile-linux-x86"
			amd64:       "namespace-profile-linux-x86"
		}
	}

	ci: pipelines: {
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
				tasks.checkCrdDrift,
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
				tasks.clippy,
				tasks.test,
				tasks.doctest,
				tasks.checkCrdDrift,
			]
		}
	}

	tasks: {
		checkCiDrift: schema.#Task & {
			hermetic: false
			command:  "cuenv"
			args: ["sync", "ci", "--check", "-p", "."]
			inputs: ["env.cue", schema.#ModuleFiles & {paths: [".github/workflows/waddle-operator-default.yml", ".github/workflows/waddle-operator-pullrequest.yml"]}]
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

		checkCrdDrift: schema.#Task & {
			command: "bash"
			args: ["-c", "diff -u config/crd/waddleservers.waddle.social.yaml <(cargo run -p waddle-operator --locked -- crd)"]
			inputs: ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "crates/**", "config/crd/waddleservers.waddle.social.yaml"]
		}
	}
}
