package cachebenchmark

import (
	"strings"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

// Qualify the operator CI task through the usual pipeline executor. Only the
// action digest crosses jobs. Both local caches start empty.
#BazelJob: {
	_phase: "seed" | "restore"
	name: "Operator CRD drift remote cache / \(_phase)"
	"runs-on": "namespace-profile-linux-x86"
	"timeout-minutes": 45
	permissions: {contents: "read", "id-token": "write"}
	if: "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
	if _phase == "seed" {
		outputs: digest: "${{ steps.qualify.outputs.digest }}"
	}
	if _phase == "restore" {
		needs: ["bazel-seed"]
	}
	steps: [
		{name: "Checkout", uses: "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"},
		for contributor in [wc.#Nix, wc.#FlakeHubCache]
		for task in contributor.tasks {
			name: task.label
			uses: task.provider.github.uses
			with: task.provider.github.with
		},
		{
			let setup = wc.#NamespaceBazelCache.tasks[0]
			name: setup.label
			run:  setup.script
		},
		{
			let setup = wc.#CuenvRelease.tasks[0]
			name: setup.label
			run:  strings.Replace(setup.script, "${CUENV_VERSION}", wc.#CuenvVersion, -1)
			env:  setup.env
		},
		{
			name: "Verify CRD drift cache \(_phase)"
			id:   "qualify"
			env: {
				if _phase == "seed" {CUENV_CACHE: "write"}
				if _phase == "restore" {
					CUENV_CACHE:               "read"
					CUENV_REMOTE_CACHE_UPLOAD: "false"
					EXPECTED_ACTION_DIGEST:    "${{ needs.bazel-seed.outputs.digest }}"
				}
			}
			run: "bash ci/cache-benchmark/verify-bazel.sh \(_phase)"
		},
	]
}
