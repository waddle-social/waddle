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
// Trial 1 ran unprivileged: Nix disabled its sandbox, built every
// derivation in a different random directory, and Cargo (whose crate
// identity includes the absolute workspace path) recompiled every member in
// every layer and in the lane. Layers need the sandbox's fixed /build/source,
// so trial 2 runs privileged with the sandbox required.
let _tag = "waddle-nix-layers-trial-2"
let _volumeJobRunner = [
	"nscloud-ubuntu-24.04-amd64-8x16-with-cache-with-features",
	"nscloud-cache-tag-\(_tag)",
	"nscloud-cache-size-20gb",
	"namespace-features:container.privileged=true",
]
let _volumeIf = "github.event_name == 'workflow_dispatch' && inputs.scope == 'namespace-volume'"

let _artifactIf = "github.event_name == 'workflow_dispatch' && inputs.scope == 'test-layer-artifact'"
let _download = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
let _testIf = "github.event_name == 'workflow_dispatch' && inputs.scope == 'namespace-test-volume'"

#VolumeJob: {
	_suite: *false | true
	_explicit: *false | true
	_tagSuffix: *"" | string
	_probeName: *"Build the Clippy lane from the volume" | string
	_artifact: *"nix-volume" | string
	_probeScript: *"ci-nix-volume-probe.py" | string
	_phase: string
	_edit:  *true | false
	if:     string
	needs?: [...string]
	outputs?: [string]: string
	name:   "\(_artifact)/\(_phase)"
	if !_suite {
		"runs-on": _volumeJobRunner
		"timeout-minutes": 90
	}
	if _suite {
		if !_explicit {_artifact: "nix-test-volume"}
		if _explicit {_artifact: "nix-test-artifact"}
		_probeName: "Build and run the production default-feature test suite"
		_probeScript: "ci-nix-test-volume.py"
		if _explicit {
			"runs-on": ["nscloud-ubuntu-24.04-amd64-16x32-with-features", "namespace-features:container.privileged=true"]
		}
		if !_explicit {"runs-on": [
			"nscloud-ubuntu-24.04-amd64-16x32-with-cache-with-features",
			"nscloud-cache-tag-waddle-test-\(_tagSuffix)",
			"nscloud-cache-size-50gb",
			"namespace-features:container.privileged=true",
			if _phase != "seed" {"nscloud-cache-exp-do-not-commit"},
		]}
		"timeout-minutes": 230
	}
	permissions: contents: "read"
	steps: [
		{name: "Checkout", uses: _checkout},
		if !_explicit {
			// Before Nix: /nix must come from the volume.
			name: "Mount Namespace Nix volume"
			uses: _volume
			with: cache: "nix"
		},
		if !_explicit {
			name: "Record volume contents"
			run:  "python3 scripts/ci-nix-volume-probe.py volume --phase \(_phase)"
		},
		if !_explicit {
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
			// Keep multi-GB Cargo target directories off the /nix volume. Nix
			// refuses a world-writable build directory such as /tmp.
			name: "Create a root-owned build directory outside the volume"
			run:  "sudo install -d -m 0755 -o root -g root /var/lib/nix-build"
		},
		{
			name: "Install Determinate Nix"
			uses: _nix.uses
			with: "extra-conf": """
				accept-flake-config = true
				substituters = https://cache.nixos.org/
				build-dir = /var/lib/nix-build
				sandbox = true
				sandbox-fallback = false
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
		if _explicit && _phase == "warm" {
			name: "Download member layer artifact"
			uses: _download
			with: {
				"artifact-ids": "${{ needs.test-artifact-seed.outputs.payload }}"
				path: "${{ runner.temp }}/rust-layer-cache"
				"merge-multiple": true
				"digest-mismatch": "error"
			}
		},
		if _explicit && _phase == "warm" {
			name: "Verify and import member layer"
			env: EXPECTED_CLOSURE: "${{ needs.test-artifact-seed.outputs.closure }}"
			run: """
				python3 scripts/ci-nix-test-volume.py require-empty
				bash scripts/ci-test-layer-transfer.sh restore "$(nix eval --raw .#checks.x86_64-linux.waddle-server-default-test.cargoArtifacts.outPath)"
				"""
		},
		{
			name: _probeName
			run:  "python3 scripts/\(_probeScript) probe --phase \(_phase)\([if _edit {" --edit"}, ""][0])"
		},
		if _explicit && _phase == "seed" {
			name: "Export member layer closure"
			id: "export"
			run: #"bash scripts/ci-test-layer-transfer.sh export "$(nix eval --raw .#checks.x86_64-linux.waddle-server-default-test.cargoArtifacts.outPath)""#
		},
		if _explicit && _phase == "seed" {
			name: "Upload member layer artifact"
			id: "payload"
			uses: _upload
			with: {
				name: "rust-member-layer-${{ github.run_attempt }}"
				path: "${{ runner.temp }}/rust-layer-cache"
				"compression-level": 0
				"retention-days": 1
				"if-no-files-found": "error"
			}
		},
		{
			name: "Upload probe measurements"
			if:   "${{ always() }}"
			uses: _upload
			with: {
				name: "\(_artifact)-\(_phase)"
				if _suite {overwrite: true}
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
	// The main pipeline's profile runner is unprivileged, so Nix falls back
	// to unsandboxed builds and member layers never match. Check that the
	// documented profile option gives a working, required sandbox.
	"profile-sandbox": {
		name:              "nix-volume/profile-sandbox"
		if:                "${{ \(_volumeIf) }}"
		"runs-on":         "namespace-profile-linux-x86;container.privileged=true"
		"timeout-minutes": 15
		permissions: contents: "read"
		steps: [
			{name: "Checkout", uses: _checkout},
			{
				name: "Install Determinate Nix"
				uses: _nix.uses
				with: "extra-conf": """
					sandbox = true
					sandbox-fallback = false
					"""
			},
			{
				name: "Build a derivation that only succeeds inside the sandbox"
				// The sandbox runs builds at /build; unsandboxed builds use a
				// random directory under the build-dir.
				run: """
					set -euo pipefail
					nix build --no-link --print-build-logs --impure --expr 'derivation { name = "sandbox-probe-${toString builtins.currentTime}"; system = "x86_64-linux"; builder = "/bin/sh"; args = ["-c" "test $PWD = /build && echo sandboxed > $out"]; }'
					"""
			},
		]
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
				uses: _download
				with: {pattern: "nix-volume-*", path: ".ci/nix-volume-results", "digest-mismatch": "error"}
			},
			{
				name: "Summarize"
				run:  "python3 scripts/test-ci-nix-volume-probe.py && python3 scripts/ci-nix-volume-probe.py summary --input .ci/nix-volume-results"
			},
		]
	}
}

let _seedTag = "${{ github.run_id }}-${{ github.run_attempt }}"
let _restoreTag = "${{ github.run_id }}-${{ needs.test-volume-seed.outputs.attempt }}"

workflow: jobs: {
	"test-volume-seed": #VolumeJob & {
		_suite: true, _phase: "seed", _edit: false, _tagSuffix: _seedTag
		if: "${{ \(_testIf) }}"
		outputs: attempt: "${{ github.run_attempt }}"
	}
	"test-volume-cold": #VolumeJob & {
		_suite: true, _phase: "cold", _tagSuffix: "\(_seedTag)-cold"
		if: "${{ \(_testIf) }}"
	}
	"test-volume-immediate": #VolumeJob & {
		_suite: true, _phase: "immediate", _tagSuffix: _restoreTag
		needs: ["test-volume-seed"]
		if: "${{ \(_testIf) }}"
	}
	"test-volume-wait": {
		name: "nix-test-volume/wait-600s"
		needs: ["test-volume-seed"]
		if: "${{ \(_testIf) }}"
		"runs-on": "ubuntu-24.04"
		"timeout-minutes": 15
		permissions: {}
		steps: [{name: "Allow the seed volume to replicate", run: "sleep 600"}]
	}
	"test-volume-delayed": #VolumeJob & {
		_suite: true, _phase: "delayed", _tagSuffix: _restoreTag
		needs: ["test-volume-seed", "test-volume-wait"]
		if: "${{ \(_testIf) }}"
	}
	"test-artifact-seed": #VolumeJob & {
		_suite: true, _explicit: true, _phase: "seed", _edit: false
		if: "${{ \(_artifactIf) }}"
		outputs: {closure: "${{ steps.export.outputs.closure }}", payload: "${{ steps.payload.outputs.artifact-id }}"}
	}
	"test-artifact-cold": #VolumeJob & {
		_suite: true, _explicit: true, _phase: "cold"
		if: "${{ \(_artifactIf) }}"
	}
	"test-artifact-warm": #VolumeJob & {
		_suite: true, _explicit: true, _phase: "warm"
		needs: ["test-artifact-seed"]
		if: "${{ \(_artifactIf) }}"
	}
	"test-volume-summary": #SuiteSummary & {
		_kind: "volume"
		needs: ["test-volume-seed", "test-volume-cold", "test-volume-immediate", "test-volume-delayed"]
		if: "${{ !cancelled() && \(_testIf) }}"
	}
	"test-artifact-summary": #SuiteSummary & {
		_kind: "artifact"
		needs: ["test-artifact-seed", "test-artifact-cold", "test-artifact-warm"]
		if: "${{ !cancelled() && \(_artifactIf) }}"
	}
}

#SuiteSummary: {
		_kind: "volume" | "artifact"
		needs: [...string]
		if: string
		name: "nix-test-\(_kind)/summary"
		"runs-on": "ubuntu-24.04"
		"timeout-minutes": 5
		permissions: {contents: "read", actions: "read"}
		steps: [
			{name: "Checkout", uses: _checkout},
			{
				name: "Download measurements"
				uses: _download
				with: {pattern: "nix-test-\(_kind)-*", path: ".ci/test-volume-results", "digest-mismatch": "error"}
			},
			{
				name: "Compare test inventories, crate reuse and job durations"
				env: GH_TOKEN: "${{ github.token }}"
				run: """
					gh api --paginate --slurp "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID/jobs?filter=latest&per_page=100" > .ci/test-volume-jobs.json
					python3 scripts/ci-nix-test-volume.py summary --transport \(_kind) --input .ci/test-volume-results --jobs .ci/test-volume-jobs.json
					"""
			},
		]
	}
