package cachebenchmark

import wc "github.com/waddle-social/waddle/ci/contributors"

// Namespace-only qualification for the workspace member layers. No FlakeHub,
// no Hestia: cache.nixos.org is the only substituter, so every Waddle output
// a probe does not find on its Namespace volume is rebuilt. The seed builds
// the Clippy lane; each probe lands on whichever machine Namespace allocates,
// records what the volume holds, and rebuilds the lane after a one-line
// waddle-server edit. Spaced probes measure how quickly a committed volume
// generation reaches other machines (Namespace distributes it in the
// background and gives no bound).
// `let` is file-scoped: the same pins as workflow.cue.
let _nix = wc.#Nix.tasks[0].provider.github
let _volume = "namespacelabs/nscloud-cache-action@1124a6f3ce44e5cf84cc22111530961f4d2a15f9" // v1
let _checkout = "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
let _upload = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
let _tag = "waddle-nix-layers-trial-1"
let _volumeJobRunner = [
	"nscloud-ubuntu-24.04-amd64-8x16-with-cache",
	"nscloud-cache-tag-\(_tag)",
	"nscloud-cache-size-20gb",
]
let _volumeIf = "github.event_name == 'workflow_dispatch' && inputs.scope == 'namespace-volume'"

#VolumeJob: {
	_phase: string
	_edit:  *true | false
	if:     string
	needs?: [...string]
	name:   "nix-volume/\(_phase)"
	"runs-on": _volumeJobRunner
	"timeout-minutes": 90
	permissions: contents: "read"
	steps: [
		{name: "Checkout", uses: _checkout},
		{
			// Before Nix: /nix must come from the volume.
			name: "Mount Namespace Nix volume"
			uses: _volume
			with: cache: "nix"
		},
		{
			name: "Record volume contents"
			run:  "python3 scripts/ci-nix-volume-probe.py volume --phase \(_phase)"
		},
		{
			name: "Prepare retained Namespace store for fresh host installation"
			// Same receipt relocation as the provider benchmark; never reinstall.
			run: """
				set -euo pipefail
				if ! sudo test -f /nix/receipt.json; then
				  echo "No cached installation receipt; continue with fresh installation."
				  exit 0
				fi
				if command -v nix >/dev/null 2>&1 || sudo test -e /usr/local/bin/determinate-nixd; then
				  echo "::error::Unexpected existing host Nix installation; refusing receipt recovery."
				  exit 1
				fi
				receipt_backup="$(mktemp -d "$RUNNER_TEMP/nix-volume-receipt.XXXXXX")"
				sudo mv /nix/receipt.json "$receipt_backup/receipt.json"
				"""
		},
		{
			name: "Install Determinate Nix"
			uses: _nix.uses
			with: "extra-conf": """
				accept-flake-config = true
				substituters = https://cache.nixos.org/
				build-dir = /tmp
				"""
		},
		{
			name: "Verify Nix daemon without building"
			run: """
				for attempt in 1 2 3 4 5; do
				  if timeout 5s nix store ping --store daemon; then
				    exit 0
				  fi
				  sleep 1
				done
				echo "::error::Nix daemon did not become ready."
				exit 1
				"""
		},
		{
			name: "Build the Clippy lane from the volume"
			run:  "python3 scripts/ci-nix-volume-probe.py probe --phase \(_phase)\([if _edit {" --edit"}, ""][0])"
		},
		{
			name: "Upload probe measurements"
			if:   "${{ always() }}"
			uses: _upload
			with: {
				name: "nix-volume-\(_phase)"
				path: ".ci/nix-volume"
				"include-hidden-files": true
				"if-no-files-found":    "error"
			}
		},
	]
}

#VolumeWait: {
	_seconds: int
	_after:   string
	name:     "nix-volume/wait-\(_seconds)s"
	needs: [_after]
	if:                "${{ !cancelled() && \(_volumeIf) }}"
	"runs-on":         "ubuntu-24.04"
	"timeout-minutes": 60
	permissions: {}
	steps: [{name: "Let Namespace distribute the committed generation", run: "sleep \(_seconds)"}]
}

workflow: jobs: {
	"volume-seed": #VolumeJob & {
		_phase: "0-seed"
		_edit:  false
		if:     "${{ \(_volumeIf) }}"
	}
	"volume-probe-1": #VolumeJob & {
		_phase: "1-immediate"
		needs: ["volume-seed"]
		if: "${{ !cancelled() && \(_volumeIf) }}"
	}
	"volume-wait-1": #VolumeWait & {_seconds: 600, _after: "volume-probe-1"}
	"volume-probe-2": #VolumeJob & {
		_phase: "2-after-10m"
		needs: ["volume-wait-1"]
		if: "${{ !cancelled() && \(_volumeIf) }}"
	}
	"volume-wait-2": #VolumeWait & {_seconds: 1800, _after: "volume-probe-2"}
	"volume-probe-3": #VolumeJob & {
		_phase: "3-after-30m"
		needs: ["volume-wait-2"]
		if: "${{ !cancelled() && \(_volumeIf) }}"
	}
	"volume-summary": {
		name: "nix-volume/summary"
		needs: ["volume-seed", "volume-probe-1", "volume-probe-2", "volume-probe-3"]
		if:                "${{ !cancelled() && \(_volumeIf) }}"
		"runs-on":         "ubuntu-24.04"
		"timeout-minutes": 5
		permissions: contents: "read"
		steps: [
			{name: "Checkout", uses: _checkout},
			{
				name: "Download measurements"
				uses: "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
				with: {pattern: "nix-volume-*", path: ".ci/nix-volume-results", "digest-mismatch": "error"}
			},
			{
				name: "Summarize"
				run:  "python3 scripts/test-ci-nix-volume-probe.py && python3 scripts/ci-nix-volume-probe.py summary --input .ci/nix-volume-results"
			},
		]
	}
}
