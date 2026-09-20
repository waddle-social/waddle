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
	{number: 1, name: "nixTestShard1", task: server.tasks.nixTestShard1},
	{number: 2, name: "nixTestShard2", task: server.tasks.nixTestShard2},
	{number: 3, name: "nixTestShard3", task: server.tasks.nixTestShard3},
	{number: 4, name: "nixTestShard4", task: server.tasks.nixTestShard4},
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
	permissions: {}
	jobs: {
		"nixTestArchive-builder": {
			name:              "nixTestArchive (builder)"
			"runs-on":         "nscloud-ubuntu-24.04-amd64-32x64;job.priority=1"
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
						"""
				},
				{
					name:                "Compile and export test archive"
					shell:               "bash"
					run:                 _archiveTask.args[1]
					"working-directory": "server"
				},
				for shard in _shards {
					name: "Upload raw test archive \(shard.number)"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" // v7.0.1
					with: {
						// Raw artifact names are their unique file basenames.
						archive:                false
						path:                   "server/.ci/nextest-archive/shard-\(shard.number)/archive-\(shard.number).nar"
						"if-no-files-found":    "error"
						"include-hidden-files": true
					}
				},
				for shard in _shards {
					name: "Upload test archive \(shard.number) metadata"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" // v7.0.1
					with: {
						name:                   "nixTestArchive-\(shard.number)-metadata"
						path:                   """
							server/.ci/nextest-archive/shard-\(shard.number)/archive-path
							server/.ci/nextest-archive/shard-\(shard.number)/archive-references
							server/.ci/nextest-archive/shard-\(shard.number)/archive-content-checksums
							server/.ci/nextest-archive/shard-\(shard.number)/archive-checksums
							"""
						"if-no-files-found":    "error"
						"include-hidden-files": true
					}
				},
				{
					name: "Upload compiler and coverage diagnostics"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a" // v7.0.1
					with: {
						name: "nixTestArchive-diagnostics"
						path: """
							server/.ci/nextest-archive/cargo-timing.html
							server/.ci/nextest-archive/partition-coverage.json
							server/.ci/nextest-archive/plan.json
							"""
						"if-no-files-found":    "error"
						"include-hidden-files": true
					}
				},
			]])
		}
		for shard in _shards {
			(shard.name): {
				name: shard.name
				// Keep the measured 8 CPU / 16 GB shape and prioritize the
				// critical test path when runner capacity is constrained.
				"runs-on": "nscloud-ubuntu-24.04-amd64-8x16;job.priority=1"
				needs: ["nixTestArchive-builder"]
				"timeout-minutes": 30
				permissions: {contents: "read", "id-token": "write"}
				steps: list.Concat([_setupSteps, [
					{
						name: "Download test archive metadata"
						id:   "archive-metadata"
						uses: "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c" // v8.0.1; Node 24, runner >= 2.327.1
						with: {
							// Omitting run-id and github-token restricts this to the
							// current workflow run's immutable producer artifact.
							name:              "nixTestArchive-\(shard.number)-metadata"
							path:              "server/.ci/nextest-archive/builder"
							"digest-mismatch": "error"
						}
					},
					{
						name:                "Restore and verify archive from binary caches"
						id:                  "archive-cache"
						shell:               "bash"
						"working-directory": "server"
						env: METADATA_DOWNLOAD: "${{ steps.archive-metadata.outputs.download-path }}"
						run: """
							set -euo pipefail
							if [[ -z "$METADATA_DOWNLOAD" ]]; then
							  echo "Metadata download exited without reporting completion; no Nix operation attempted" >&2
							  exit 1
							fi
							bash scripts/nextest-archive-transfer.sh validate .ci/nextest-archive/builder \(shard.number)
							expected="$(nix eval --raw ../#waddle-server-test-archive.shard\(shard.number).outPath)"
							if bash scripts/nextest-archive-transfer.sh cache "$expected" .ci/nextest-archive/builder \(shard.number); then
							  echo "hit=true" >> "$GITHUB_OUTPUT"
							else
							  status=$?
							  if [[ "$status" -ne 2 ]]; then exit "$status"; fi
							  echo "hit=false" >> "$GITHUB_OUTPUT"
							fi
							"""
					},
					{
						name: "Download raw test archive"
						id:   "archive-payload"
						if:   "${{ steps.archive-cache.outputs.hit == 'false' }}"
						uses: "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c" // v8.0.1
						with: {
							name:              "archive-\(shard.number).nar"
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
							CACHE_HIT:         "${{ steps.archive-cache.outputs.hit }}"
						}
						run: #"""
							if [[ -z "$METADATA_DOWNLOAD" ]]; then
							  echo "Metadata download exited without reporting completion" >&2
							  exit 1
							fi
							if [[ "$CACHE_HIT" == true ]]; then exit 0; fi
							if [[ "$CACHE_HIT" != false || -z "$PAYLOAD_DOWNLOAD" ]]; then
							  echo "Artifact download exited without reporting completion; no Nix import attempted" >&2
							  exit 1
							fi
							"""#
					},
					{
						name:  "Run test shard"
						shell: "bash"
						env: ARCHIVE_TRANSFER_MODE: "${{ steps.archive-cache.outputs.hit == 'true' && 'verify' || 'import' }}"
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
