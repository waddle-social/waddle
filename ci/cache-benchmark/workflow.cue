package cachebenchmark

import (
	"list"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

let _nix = wc.#Nix.tasks[0].provider.github
let _fh = wc.#FlakeHubCache.tasks[0].provider.github
let _runner = ["nscloud-ubuntu-24.04-amd64-8x16"]
let _volumeRunner = [
	"nscloud-ubuntu-24.04-amd64-8x16-with-cache",
	// Keep the first experiment's isolated identity across retries so
	// Namespace can distribute its population across the fleet. Not cold.
	"nscloud-cache-tag-waddle-ci-benchmark-35510413179",
	"nscloud-cache-size-20gb",
]
let _h2 = "Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527"
let _h3 = "Mic92/hestia@f1f4df2801140a36398ed423533c8460618539df" // v3.0.1
let _magic = "DeterminateSystems/magic-nix-cache-action@2cdbb78a6eed25b3bc8b97f863549be5f6567e55"
let _snapshot = "nix-community/cache-nix-action@7df957e333c1e5da7721f60227dbba6d06080569" // v7
let _volume = "namespacelabs/nscloud-cache-action@1124a6f3ce44e5cf84cc22111530961f4d2a15f9" // v1
let _checkout = "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
let _upload = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"

#BenchmarkJob: {
	_phase: "existing" | "seed" | "warm"
	_hestiaSeeded: *false | true
	name: "cache-benchmark/\(_phase)/${{ matrix.variant }}"
	"runs-on": "${{ matrix.runner }}"
	"timeout-minutes": 20
	if _phase == "existing" {needs: ["normal-ci"]}
	if _phase == "seed" && !_hestiaSeeded {
		needs: ["normal-ci", "existing"]
		if: "${{ !cancelled() && needs.normal-ci.result == 'success' }}"
	}
	if _phase == "warm" && !_hestiaSeeded {
		needs: ["normal-ci", "seed"]
		if: "${{ !cancelled() && needs.normal-ci.result == 'success' }}"
	}
	if _hestiaSeeded {
		if: "${{ !cancelled() && needs.normal-ci.result == 'success' }}"
		if _phase == "seed" {
			needs: ["normal-ci", "warm"]
			outputs: "manifest-version": "${{ steps.hestia-seed.outputs.manifest-version }}"
		}
		if _phase == "warm" {needs: ["normal-ci", "h3-seed"]}
	}
	strategy: {
		"fail-fast": false
		"max-parallel": 1
		matrix: include: [
			if _phase == "existing" {
				variant: "fh-h2", provider: "fh-h2", runner: _runner
			},
			if _phase == "existing" {variant: "fh", provider: "fh", runner: _runner},
			if _phase == "existing" {variant: "h2", provider: "h2", runner: _runner},
			if _phase == "existing" {variant: "h3", provider: "h3", runner: _runner},
			if _phase == "existing" {variant: "fh-h3", provider: "fh-h3", runner: _runner},
			if _phase != "existing" && !_hestiaSeeded {
				variant: "namespace"
				if _phase == "seed" {provider: "fh", runner: _volumeRunner}
				if _phase == "warm" {
					provider: "namespace"
					// A miss must not replace a populated seed with an empty store.
					runner: list.Concat([_volumeRunner, ["nscloud-cache-exp-do-not-commit"]])
				}
			},
			if _phase != "existing" && !_hestiaSeeded {
				variant: "cache-nix", runner: _runner
				if _phase == "seed" {provider: "fh"}
				if _phase == "warm" {provider: "cache-nix"}
			},
			if _phase != "existing" && !_hestiaSeeded {
				variant: "magic", runner: _runner
				if _phase == "seed" {provider: "magic-seed"}
				if _phase == "warm" {provider: "magic"}
			},
			if _hestiaSeeded {
				variant: "h3-seeded", runner: _runner
				if _phase == "seed" {provider: "fh"}
				if _phase == "warm" {provider: "h3"}
			},
		]
	}
	steps: [
		{name: "Checkout", uses: _checkout},
		{
			name: "Start setup measurement"
			run: "python3 scripts/ci-cache-benchmark.py mark --output .ci/cache-benchmark"
		},
		{
			name: "Mount isolated Namespace Nix volume"
			if: "${{ matrix.variant == 'namespace' }}"
			uses: _volume
			with: cache: "nix"
		},
		{name: "Install Determinate Nix", uses: _nix.uses, with: _nix.with},
		{
			name: "Restore or seed isolated Nix store snapshot"
			if: "${{ matrix.variant == 'cache-nix' }}"
			uses: _snapshot
			with: {
				"primary-key": "waddle-nix-benchmark-${{ github.run_id }}"
				// Remove the implicit /nix root before selecting store + database.
				// A child-only negation would still let tar recurse into auth state.
				paths: """
					!/nix
					/nix/store
					/nix/var/nix/db
					"""
				purge: false
				save: _phase == "seed"
			}
		},
		{
			name: "Setup FlakeHub Cache"
			if: "${{ matrix.provider == 'fh' || matrix.provider == 'fh-h2' || matrix.provider == 'fh-h3' }}"
			uses: _fh.uses
			with: _fh.with
		},
		{
			name: "Setup Hestia 2"
			if: "${{ matrix.provider == 'h2' || matrix.provider == 'fh-h2' }}"
			uses: _h2
			with: {
				version: "v2.0.0"
				listen: "127.0.0.1:37516"
				"upstream-cache-filter": true
				"drain-timeout": 300
			}
		},
		{
			name: "Setup Hestia 3"
			if: "${{ matrix.provider == 'h3' || matrix.provider == 'fh-h3' || matrix.variant == 'h3-seeded' }}"
			uses: _h3
			with: {
				version: "v3.0.1"
				listen: "127.0.0.1:37516"
				"upstream-cache-filter": true
				if _hestiaSeeded && _phase == "seed" {"drain-timeout": 30}
				if !_hestiaSeeded || _phase != "seed" {"drain-timeout": 300}
				if _hestiaSeeded && _phase == "warm" {
					"wait-manifest-version": "${{ needs.h3-seed.outputs.manifest-version || '0' }}"
				}
			}
		},
		{
			name: "Setup Magic GHA cache"
			if: "${{ matrix.variant == 'magic' }}"
			uses: _magic
			with: {
				"use-gha-cache": "enabled"
				"use-flakehub": *"disabled" | "enabled"
				if _phase == "seed" {"use-flakehub": "enabled"}
				"diff-store": _phase == "seed"
				listen: "127.0.0.1:37515"
				"source-revision": "5a7f052c8f0c6511bbef3570eeed4d39242171ef"
			}
		},
		{
			name: "Restore dependencies, vendor and exact prior archive without building"
			env: {
				BENCHMARK_PROVIDER: "${{ matrix.provider }}"
				BENCHMARK_VARIANT: "${{ matrix.variant }}"
			}
			run: """
				set +e
				python3 scripts/ci-cache-benchmark.py restore --provider "$BENCHMARK_PROVIDER" --variant "$BENCHMARK_VARIANT" --phase \(_phase) --output .ci/cache-benchmark
				status=$?
				if [ "$status" -eq 2 ]; then
				  echo "Cache miss recorded; this candidate is unqualified, and no build was attempted."
				  exit 0
				fi
				exit "$status"
				"""
		},
		if _hestiaSeeded && _phase == "seed" {
			name: "Register and upload the exact Hestia seed outputs"
			id: "hestia-seed"
			run: "python3 scripts/ci-cache-benchmark.py seed-hestia --output .ci/cache-benchmark"
		},
		{
			name: "Upload cache measurements"
			if: "${{ always() }}"
			uses: _upload
			with: {
				name: "cache-benchmark-\(_phase)-${{ matrix.variant }}"
				path: ".ci/cache-benchmark"
				"include-hidden-files": true
				"if-no-files-found": "error"
			}
		},
	]
}

workflow: {
	name: "waddle-ci-cache-benchmark"
	on: {
		pull_request: paths: ["ci/cache-benchmark/**", "scripts/*cache-benchmark*", "docs/ci-cache-benchmarks.md"]
		workflow_dispatch: {}
	}
	permissions: {contents: "read", actions: "read", "id-token": "write"}
	concurrency: {
		group: "${{ github.workflow }}-${{ github.head_ref || github.ref }}"
		"cancel-in-progress": true
	}
	jobs: {
		"normal-ci": {
			name: "Wait for normal CI before cache qualification"
			"runs-on": "ubuntu-24.04"
			"timeout-minutes": 65
			steps: [
				{name: "Checkout", uses: _checkout},
				{
					name: "Validate qualification contracts"
					run: "python3 scripts/test-ci-cache-benchmark.py"
				},
				{
					name: "Wait for existing workflows to finish"
					env: {
						GITHUB_TOKEN: "${{ github.token }}"
						BENCHMARK_HEAD_SHA: "${{ github.event.pull_request.head.sha || github.sha }}"
					}
					run: "python3 scripts/ci-cache-benchmark.py wait-for-ci --sha \"$BENCHMARK_HEAD_SHA\""
				},
			]
		}
		existing: #BenchmarkJob & {_phase: "existing"}
		seed: #BenchmarkJob & {_phase: "seed"}
		warm: #BenchmarkJob & {_phase: "warm"}
		"h3-seed": #BenchmarkJob & {_phase: "seed", _hestiaSeeded: true}
		"h3-warm": #BenchmarkJob & {_phase: "warm", _hestiaSeeded: true}
		summary: {
			name: "Cache qualification report"
			needs: ["existing", "seed", "warm", "h3-seed", "h3-warm"]
			if: "${{ !cancelled() }}"
			"runs-on": "ubuntu-24.04"
			"timeout-minutes": 5
			steps: [
				{name: "Checkout", uses: _checkout},
				{
					name: "Download measurements"
					uses: "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
					with: {pattern: "cache-benchmark-*", path: ".ci/cache-results", "digest-mismatch": "error"}
				},
				{
					name: "Include cache post-job and queue timings"
					env: GITHUB_TOKEN: "${{ github.token }}"
					run: "python3 scripts/ci-cache-benchmark.py summarize --input .ci/cache-results --output .ci/cache-summary"
				},
				{
					name: "Upload qualification report"
					uses: _upload
					with: {name: "cache-qualification-summary", path: ".ci/cache-summary", "include-hidden-files": true, "if-no-files-found": "error"}
				},
			]
		}
	}
}
