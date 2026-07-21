package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

let _rootSyncInputs = [
	"../../.github/workflows/**",
	"../../env.cue",
	"../../**/env.cue",
	"../../cuenv.lock",
	"../../cue.mod/**",
	"../../flake.lock",
	"../../flake.nix",
	"../../.rules.cue",
	"../../.gitignore",
	"../../server/scripts/check-root-sync-drift.mjs",
	"../../server/scripts/check-root-sync-drift.test.ts",
	"env.cue",
	"run-root-sync-guard.sh",
	"run-root-sync-guard.test.ts",
]

schema.#Project & {
	name: "waddle-root-sync"
	let _t = tasks

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	config: ci: cuenv: {
		source:  "release"
		version: "0.54.0"
	}

	ci: providers: ["github"]
	ci: contributors: [
		c.#Nix,
		c.#CuenvRelease,
		schema.#Contributor & {
			id: "rootSyncGuard"
			when: always: true
			tasks: [{
				id:       "run"
				label:    "Run root sync guard"
				priority: 20
				script:   "bash ci/root-sync/run-root-sync-guard.sh"
				inputs:   _rootSyncInputs
				dependsOn: [
					"cuenv:contributor:nix.install",
					"cuenv:contributor:cuenv.setup",
				]
			}]
		},
	]

	ci: provider: github: runner: "ubuntu-latest"
	ci: pipelines: {
		default: {
			mode:        "expanded"
			derivePaths: true
			when: {
				branch: ["main"]
				defaultBranch: true
				manual: true
			}
			tasks: [_t.requireGuardMarker]
		}
		pullRequest: {
			mode:        "expanded"
			derivePaths: true
			when: pullRequest: true
			tasks: [_t.requireGuardMarker]
		}
	}

	tasks: {
		requireGuardMarker: schema.#Task & {
			command: "bash"
			args: ["-ceu", #"""
				marker=.root-sync-guard-passed
				test -f "$marker"
				rm -- "$marker"
				test ! -e "$marker"
			"""#]
			hermetic: false
			cache: mode: "never"
			inputs: _rootSyncInputs
		}
	}
}
