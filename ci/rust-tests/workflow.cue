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
let _archiveTask = server.tasks.nixTestArchive
let _shards = [
	{name: "nixTestShard1", task: server.tasks.nixTestShard1},
	{name: "nixTestShard2", task: server.tasks.nixTestShard2},
	{name: "nixTestShard3", task: server.tasks.nixTestShard3},
	{name: "nixTestShard4", task: server.tasks.nixTestShard4},
]

// Retain the old workflow's task inputs and cuenv's implicit schema/module
// triggers. Normalize parent-relative paths before passing them to GitHub.
let _paths = {
	for input in _archiveTask.inputs {
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
	permissions: {
		contents:        "read"
		checks:          "write"
		"pull-requests": "none"
		packages:        "read"
		"id-token":      "write"
	}
	jobs: {
		"nixTestArchive-builder": {
			name:              "nixTestArchive (builder)"
			"runs-on":         "nscloud-ubuntu-24.04-amd64-32x64;job.priority=1"
			"timeout-minutes": 60
			steps: list.Concat([_setupSteps, [
				{
					name:  "Validate CI helper contracts"
					shell: "bash"
					run: """
						python3 tests/ci-timings-test.py
						python3 server/scripts/test_check_nextest_shards.py
						python3 server/scripts/test_nextest_archive_transfer.py
						"""
				},
				{
					name:                "Compile and export test archive"
					shell:               "bash"
					run:                 _archiveTask.args[1]
					"working-directory": "server"
				},
				{
					name: "Upload raw test archive"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" // v7.0.1; Node 24, runner >= 2.327.1
					with: {
						// Raw uploads use the file basename as their artifact name.
						// Avoid wrapping the already-compressed multi-GB NAR in ZIP.
						archive:                false
						path:                   "server/.ci/nextest-archive/archive.nar"
						"if-no-files-found":    "error"
						"include-hidden-files": true
					}
				},
				{
					name: "Upload test archive metadata"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" // v7.0.1
					with: {
						name: "nixTestArchive-metadata"
						path: """
							server/.ci/nextest-archive/archive-path
							server/.ci/nextest-archive/archive-references
							server/.ci/nextest-archive/archive-checksums
							server/.ci/nextest-archive/cargo-timing.html
							server/.ci/nextest-archive/partition-coverage.json
							"""
						"if-no-files-found":    "error"
						"include-hidden-files": true
					}
				},
			]])
		}
		for shard in _shards {
			(shard.name): {
				name:      shard.name
				"runs-on": "namespace-profile-linux-x86"
				needs: ["nixTestArchive-builder"]
				"timeout-minutes": 30
				steps: list.Concat([_setupSteps, [
					{
						name: "Download test archive metadata"
						id:   "archive-metadata"
						uses: "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c" // v8.0.1; Node 24, runner >= 2.327.1
						with: {
							// Omitting run-id and github-token restricts this to the
							// current workflow run's immutable producer artifact.
							name:              "nixTestArchive-metadata"
							path:              "server/.ci/nextest-archive/builder"
							"digest-mismatch": "error"
						}
					},
					{
						name: "Download raw test archive"
						id:   "archive-payload"
						uses: "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c" // v8.0.1
						with: {
							name:              "archive.nar"
							path:              "server/.ci/nextest-archive/builder"
							"skip-decompress": true
							"digest-mismatch": "error"
						}
					},
					{
						name:  "Require completed artifact downloads"
						shell: "bash"
						env: {
							METADATA_DOWNLOAD: "${{ steps.archive-metadata.outputs.download-path }}"
							PAYLOAD_DOWNLOAD:  "${{ steps.archive-payload.outputs.download-path }}"
						}
						run: #"""
							if [[ -z "$METADATA_DOWNLOAD" || -z "$PAYLOAD_DOWNLOAD" ]]; then
							  echo "Artifact download exited without reporting completion; no Nix import attempted" >&2
							  exit 1
							fi
							"""#
					},
					{
						name:                "Run test shard"
						shell:               "bash"
						run:                 shard.task.args[1]
						"working-directory": "server"
					},
				]])
			}
		}
		nixTest: {
			name:              "nixTest"
			"runs-on":         "ubuntu-24.04"
			"timeout-minutes": 5
			// A failed, skipped, canceled, or absent worker must fail the
			// stable gate instead of leaving it skipped and mergeable.
			if: "${{ always() }}"
			needs: list.Concat([["nixTestArchive-builder"], [for shard in _shards {shard.name}]])
			steps: [{
				name:  "Require archive and all four test shards"
				shell: "bash"
				env: {
					ARCHIVE_RESULT: "${{ needs['nixTestArchive-builder'].result }}"
					SHARD_1_RESULT: "${{ needs.nixTestShard1.result }}"
					SHARD_2_RESULT: "${{ needs.nixTestShard2.result }}"
					SHARD_3_RESULT: "${{ needs.nixTestShard3.result }}"
					SHARD_4_RESULT: "${{ needs.nixTestShard4.result }}"
				}
				run: #"""
					set -euo pipefail
					for result in "$ARCHIVE_RESULT" "$SHARD_1_RESULT" "$SHARD_2_RESULT" "$SHARD_3_RESULT" "$SHARD_4_RESULT"; do
					  if [[ "$result" != success ]]; then
					    echo "Archive and all four test shards must succeed; received: ${result:-missing}" >&2
					    exit 1
					  fi
					done
					echo "All four test shards and inventory checks passed."
					"""#
			}]
		}
	}
}
