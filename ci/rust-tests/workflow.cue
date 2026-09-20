package rusttests

import (
	"list"
	"path"
	"strings"
	wc "github.com/waddle-social/waddle/ci/contributors"
	server "github.com/waddle-social/waddle/server:cuenv"
)

// cuenv 0.55 wraps matrix jobs in the complete development shell and cannot
// disable artifact compression. Keep the task commands and cache setup in their
// existing CUE definitions while generating this focused workflow directly.
let _testTask = server.tasks.nixTestLocal

// Retain the old workflow's task inputs and cuenv's implicit schema/module
// triggers. Normalize parent-relative paths before passing them to GitHub.
let _paths = {
	for input in _testTask.inputs {
		let normalized = path.Clean("server/\(input)")
		(normalized): true
		if !strings.Contains(input, "*") {
			"\(normalized)/**": true
		}
	}
	".github/workflows/waddle-server-rusttests.yml": true
	"server/env.cue":                                true
	"server/schema/**":                              true
	"cue.mod/**":                                    true
	"ci/rust-tests/**":                              true
	"scripts/sync-rust-tests.sh":                    true
}

let _setupSteps = [
	{
		name: "Checkout"
		uses: "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
		with: "fetch-depth": 0
	},
	for contributor in [wc.#Nix, wc.#FlakeHubCache, wc.#Hestia]
	for task in contributor.tasks {
		name: task.label
		uses: task.provider.github.uses
		with: task.provider.github.with
	},
]

workflow: {
	name: "waddle-server-rustTests"
	on: pull_request: paths: list.SortStrings([for pattern, _ in _paths {pattern}])
	concurrency: {
		group:                "${{ github.workflow }}-${{ github.head_ref || github.ref }}"
		"cancel-in-progress": true
	}
	permissions: {}
	jobs: {
		"nixTest-builder": {
			name: "nixTest (compile and four isolated shards)"
			// Namespace documents privileged runner containers for Nix mount
			// and user namespaces. Each job still has its own ephemeral VM.
			"runs-on": [
				"nscloud-ubuntu-24.04-amd64-32x64-with-features",
				"namespace-features:container.privileged=true;job.priority=1",
			]
			"timeout-minutes": 60
			permissions: {contents: "read", "id-token": "write"}
			steps: list.Concat([_setupSteps, [
				{
					name:  "Validate CI helper contracts"
					shell: "bash"
					run: """
						python3 tests/ci-timings-test.py
						python3 server/scripts/test_check_nextest_shards.py
						python3 server/scripts/test_nextest_archive_transfer.py
						python3 server/scripts/test_build_nextest_archive.py
						python3 server/scripts/test_plan_nextest_binary_shards.py
						python3 server/scripts/test_pin_nextest_shard.py
						python3 server/scripts/test_run_nextest_shards.py
						"""
				},
				{
					name:                "Compile and run four isolated test shards"
					shell:               "bash"
					run:                 "\(_testTask.command) \(_testTask.args[0])"
					"working-directory": "server"
				},
				{
					name: "Upload compiler and coverage diagnostics"
					if:   "${{ !cancelled() }}"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
					with: {
						name:                   "nixTestArchive-diagnostics"
						path:                   "server/.ci/nextest-archive"
						"if-no-files-found":    "error"
						"include-hidden-files": true
					}
				},
			]])
		}
		nixTest: {
			name:              "nixTest"
			"runs-on":         "ubuntu-24.04"
			"timeout-minutes": 5
			if:                "${{ always() }}"
			needs: ["nixTest-builder"]
			steps: [{
				name:  "Require compilation and all four isolated test shards"
				shell: "bash"
				env: TEST_RESULT: "${{ needs['nixTest-builder'].result }}"
				run: #"""
					set -euo pipefail
					if [[ "$TEST_RESULT" != success ]]; then
					  echo "Compilation and all four shards must succeed; received: ${TEST_RESULT:-missing}" >&2
					  exit 1
					fi
					echo "All four test shards and inventory checks passed."
					"""#
			}]
		}
	}
}
