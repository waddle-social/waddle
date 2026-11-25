package cachebenchmark

import wc "github.com/waddle-social/waddle/ci/contributors"

#CargoVolumeJob: {
	_phase: "seed" | "restore"
	name:   "Cargo target volume / \(_phase)"
	if:     "${{ github.event_name == 'workflow_dispatch' && inputs.scope == 'cargo' && github.ref == 'refs/heads/main' }}"
	"runs-on": [
		"nscloud-ubuntu-24.04-amd64-8x16-with-cache",
		"nscloud-cache-tag-waddle-cargo-${{ github.repository_id }}-${{ github.run_id }}-${{ github.run_attempt }}",
		"nscloud-cache-size-50gb",
		"nscloud-cache-allow-commit-from-main",
		if _phase == "restore" {"nscloud-cache-exp-do-not-commit"},
	]
	permissions: contents: "read"
	"timeout-minutes": 100
	if _phase == "seed" {
		outputs: inventory: "${{ steps.seed.outputs.inventory }}"
	}
	if _phase == "restore" {needs: ["cargo-seed"]}
	steps: [
		{
			name: "Checkout"
			uses: "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
			with: "persist-credentials": false
		},
		{
			name: "Mount isolated Cargo workspace and target"
			uses: "namespacelabs/nscloud-cache-action@1124a6f3ce44e5cf84cc22111530961f4d2a15f9"
			with: {
				path:                 "~/.cache/waddle-cargo-trial"
				"fail-on-cache-miss": _phase == "restore"
			}
		},
		{
			let setup = wc.#Nix.tasks[0]
			name: setup.label
			uses: setup.provider.github.uses
			with: setup.provider.github.with
		},
		{
			name: "Run default-feature server tests"
			id:   _phase
			if _phase == "restore" {
				env: SEED_INVENTORY: "${{ needs.cargo-seed.outputs.inventory }}"
			}
			run: "nix develop --command python3 ci/cache-benchmark/cargo_volume.py \(_phase)"
		},
	]
}
