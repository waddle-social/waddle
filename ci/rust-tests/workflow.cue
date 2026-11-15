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
let _testTask = server.tasks.nixTestShards

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
					run:   "bash server/scripts/check-ci-helper-contracts.sh"
				},
				{
					name:  "Compile and run four isolated test shards"
					shell: "bash"
					run: strings.Join(list.Concat([[_testTask.command], _testTask.args]), " ")
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
		// Capture only GitHub-redacted logs, with bounded artifact size. The known
		// failed builder makes its diagnostics available immediately on this run.
		"nixTest-failure-diagnostics": {
			name: "Previous Rust qualification failure diagnostics"
			"runs-on": "ubuntu-24.04"
			"timeout-minutes": 5
			permissions: actions: "read"
			steps: [
				{
					name: "Collect bounded GitHub-redacted failure log"
					shell: "bash"
					env: {
						GH_TOKEN: "${{ github.token }}"
						GH_REPOSITORY: "${{ github.repository }}"
					}
					run: #"""
						set -euo pipefail
						mkdir -p failure-diagnostics
						python3 - <<'PYLOG'
						import collections
						import os
						import pathlib
						import urllib.request
						import urllib.parse

						class SafeRedirect(urllib.request.HTTPRedirectHandler):
						    def redirect_request(self, req, fp, code, msg, headers, newurl):
						        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
						        if redirected and urllib.parse.urlsplit(req.full_url).netloc != urllib.parse.urlsplit(newurl).netloc:
						            redirected.remove_header("Authorization")
						        return redirected

						opener = urllib.request.build_opener(SafeRedirect())
						job_id = 132946392071
						url = f"https://api.github.com/repos/{os.environ['GH_REPOSITORY']}/actions/jobs/{job_id}/logs"
						request = urllib.request.Request(url, headers={
						    "Authorization": f"Bearer {os.environ['GH_TOKEN']}",
						    "Accept": "application/vnd.github+json",
						    "X-GitHub-Api-Version": "2022-11-28",
						})
						# GitHub serves an already-redacted plain-text job log through a
						# temporary redirect. Keep only its last 200 KiB, never the token.
						tail = bytearray()
						excerpts = bytearray()
						before = collections.deque(maxlen=40)
						after = 0
						with opener.open(request, timeout=60) as response:
						    for line in response:
						        tail.extend(line)
						        del tail[:-204800]
						        matches = any(word in line.lower() for word in (
						            b"lineage_catalog_fence", b"panicked", b"assertion",
						        ))
						        if matches:
						            if not after:
						                excerpts.extend(b"\n--- failure context ---\n" + b"".join(before))
						            after = 41
						        if after:
						            excerpts.extend(line)
						            del excerpts[:-2097152]
						            after -= 1
						        before.append(line)
						pathlib.Path(f"failure-diagnostics/job-{job_id}-tail.log").write_bytes(tail)
						pathlib.Path(f"failure-diagnostics/job-{job_id}-failure-context.log").write_bytes(excerpts)
						PYLOG
						"""#
				},
				{
					name: "Upload bounded failure log"
					uses: "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
					with: {
						name: "rust-qualification-failure-log"
						path: "failure-diagnostics"
						"if-no-files-found": "error"
						"retention-days": 3
					}
				},
			]
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
