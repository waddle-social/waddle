package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

schema.#Project & {
	name: "waddle-design"

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
		c.#BunWorkspace,
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
		pullRequest: {
			when: {
				pullRequest: true
			}
			// Light pull-request lane: free GitHub-hosted runner (public repository).
			provider: github: runner: "ubuntu-24.04"
			provider: github: permissions: {
				contents:   "read"
				"id-token": "write"
				packages:   "none"
			}
			"tasks": [tasks.test]
		}
	}

	tasks: {
		// Runs Panda codegen + cssgen on a fixture that uses the preset.
		test: schema.#Task & {
			command: "bun"
			args: ["test"]
			inputs: [
				schema.#ModuleFiles & {paths: ["package.json", "bun.lock"]},
				"package.json",
				"src/**",
				"test/**",
			]
		}
	}
}
