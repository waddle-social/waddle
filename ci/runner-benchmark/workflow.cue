package runnerbenchmark

import (
	"encoding/json"
	"list"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

// Runs the PR Rust shard lane (ci/rust-tests) on several Namespace machine
// shapes and reports what each used of the machine it asked for. Variants
// run one at a time, without `job.priority`, so the benchmark never holds
// more than one machine and ordinary CI is scheduled ahead of it.
//
// scripts/run-runner-benchmark.sh pushes a commit to a `ci-benchmark/**`
// branch to measure it. This is not a
// `pull_request` workflow on purpose: scripts/integration-ci.ts mirrors every
// one of those into the merge queue as a verification lane.
let _checkout = "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
let _upload = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
let _download = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
let _usage = "python3 scripts/ci-machine-usage.py"

// `lane` names a flake.nix benchmarkShardLanes entry. `32x64` repeats the
// production settings as its own derivation, so the baseline compiles too.
let _variants = [
	{variant: "32x64", lane: "32x64", vcpu: 32, memory: 64},
	{variant: "32x64-j16", lane: "32x64-j16", vcpu: 32, memory: 64},
	{variant: "16x32", lane: "16x32", vcpu: 16, memory: 32},
	{variant: "16x32-j8", lane: "16x32-j8", vcpu: 16, memory: 32},
	{variant: "8x32", lane: "8x32", vcpu: 8, memory: 32},
]

// The same setup as the lane being measured.
let _setupSteps = [
	{
		name: "Checkout"
		uses: _checkout
		with: "fetch-depth": 0
	},
	{
		name:  "Start machine usage sampler"
		shell: "bash"
		run:   "nohup \(_usage) sample --output \"$RUNNER_TEMP/samples.jsonl\" >/dev/null 2>&1 &"
	},
	for contributor in [wc.#Nix, wc.#FlakeHubCache, wc.#Hestia]
	for task in contributor.tasks {
		name: task.label
		uses: task.provider.github.uses
		with: task.provider.github.with
	},
]

workflow: {
	name: "waddle-ci-runner-benchmark"
	on: {
		push: branches: ["ci-benchmark/**"]
		workflow_dispatch: inputs: variants: {
			description: "Comma-separated variants to run; empty runs all"
			type:        "string"
			default:     ""
		}
	}
	concurrency: {
		group:                "${{ github.workflow }}-${{ github.ref }}"
		"cancel-in-progress": true
	}
	permissions: {}
	jobs: {
		contracts: {
			name:              "runner-benchmark/contracts"
			outputs: matrix: "${{ steps.select.outputs.matrix }}"
			"runs-on":         "ubuntu-24.04"
			"timeout-minutes": 5
			permissions: contents: "read"
			steps: [
				{name: "Checkout", uses: _checkout},
				{name: "Validate measurement contracts", run: "python3 scripts/test-ci-machine-usage.py"},
				{
					// A pushed commit selects variants with `variants=a,b` in its
					// message (scripts/run-runner-benchmark.sh writes it).
					name:  "Select variants"
					id:    "select"
					shell: "bash"
					env: {
						ALL_VARIANTS: json.Marshal(_variants)
						SELECTION:    "${{ inputs.variants && format('variants={0}', inputs.variants) || github.event.head_commit.message }}"
					}
					run: #"""
						python3 - >> "$GITHUB_OUTPUT" <<'PY'
						import json, os, re
						variants = json.loads(os.environ["ALL_VARIANTS"])
						names = [item["variant"] for item in variants]
						chosen = re.search(r"variants=([0-9a-z,-]+)", os.environ.get("SELECTION", ""))
						wanted = chosen.group(1).split(",") if chosen else names
						unknown = sorted(set(wanted) - set(names))
						if unknown:
						    raise SystemExit(f"unknown variants: {unknown}")
						print("matrix=" + json.dumps({"include": [item for item in variants if item["variant"] in wanted]}))
						PY
						"""#
				},
			]
		}
		lane: {
			name: "runner-benchmark/${{ matrix.variant }}"
			needs: ["contracts"]
			"runs-on": [
				"nscloud-ubuntu-24.04-amd64-${{ matrix.vcpu }}x${{ matrix.memory }}-with-features",
				"namespace-features:container.privileged=true",
			]
			"timeout-minutes": 75
			permissions: {contents: "read", "id-token": "write"}
			strategy: {
				"fail-fast":    false
				"max-parallel": 1
				matrix: "${{ fromJSON(needs.contracts.outputs.matrix) }}"
			}
			steps: list.Concat([_setupSteps, [
				{
					name:                "Compile and run four isolated test shards"
					id:                  "lane"
					shell:               "bash"
					"working-directory": "server"
					env: WADDLE_NEXTEST_BENCHMARK_LANE: "${{ matrix.lane }}"
					run: #"""
						set -euo pipefail
						started=$(date +%s)
						status=0
						timeout --kill-after=2m 55m bash scripts/run-nextest-shards.sh 2>&1 |
						  python3 ../scripts/ci-machine-usage.py tee --log "$RUNNER_TEMP/lane.log" || status=$?
						echo "$(($(date +%s) - started))" > "$RUNNER_TEMP/lane-seconds"
						exit "$status"
						"""#
				},
				{
					name:  "Report machine usage"
					if:    "${{ !cancelled() }}"
					shell: "bash"
					env: {
						VARIANT: "${{ matrix.variant }}"
						VCPU:    "${{ matrix.vcpu }}"
						MEMORY:  "${{ matrix.memory }}"
						OUTCOME: "${{ steps.lane.outcome }}"
					}
					run: #"""
						set -euo pipefail
						mkdir -p "$RUNNER_TEMP/report"
						touch "$RUNNER_TEMP/samples.jsonl" "$RUNNER_TEMP/lane.log"
						\#(_usage) report --samples "$RUNNER_TEMP/samples.jsonl" --log "$RUNNER_TEMP/lane.log" \
						  --variant "$VARIANT" --vcpu "$VCPU" --memory-gib "$MEMORY" --outcome "${OUTCOME:-skipped}" \
						  --lane-seconds "$(cat "$RUNNER_TEMP/lane-seconds" 2>/dev/null || echo 0)" \
						  | tee "$RUNNER_TEMP/report/$VARIANT.json"
						grep -h 'WADDLE_CI_METRIC' "$RUNNER_TEMP/lane.log" | grep -v 'phase=linker_verified' \
						  > "$RUNNER_TEMP/report/$VARIANT.metrics.txt" || true
						cp "$RUNNER_TEMP/samples.jsonl" "$RUNNER_TEMP/report/$VARIANT.samples.jsonl"
						\#(_usage) summarize "$RUNNER_TEMP/report" >> "$GITHUB_STEP_SUMMARY"
						"""#
				},
				{
					name: "Upload measurements"
					if:   "${{ !cancelled() }}"
					uses: _upload
					with: {
						name:                "runner-benchmark-${{ matrix.variant }}"
						path:                "${{ runner.temp }}/report"
						"if-no-files-found": "error"
					}
				},
			]])
		}
		summary: {
			name: "runner-benchmark/summary"
			needs: ["lane"]
			if:                "${{ !cancelled() }}"
			"runs-on":         "ubuntu-24.04"
			"timeout-minutes": 5
			permissions: contents: "read"
			steps: [
				{name: "Checkout", uses: _checkout},
				{
					name: "Download measurements"
					uses: _download
					with: {
						pattern:          "runner-benchmark-*"
						path:             "measurements"
						"merge-multiple": true
					}
				},
				{
					name: "Compare runner shapes"
					run:  "\(_usage) summarize measurements | tee -a \"$GITHUB_STEP_SUMMARY\""
				},
			]
		}
	}
}
