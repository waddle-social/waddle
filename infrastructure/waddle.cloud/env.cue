package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "waddle-cloud"

	runtime: {
		type:  "nix"
		flake: "../.."
	}

	tasks: {
		"helm-push": schema.#Task & {
			command: "bash"
			args: ["-c", "helm package ../../server/charts/waddle-server && helm package ./charts/livekit-sfu && helm push waddle-server-*.tgz oci://ghcr.io/waddle-social/waddle/charts && helm push livekit-sfu-*.tgz oci://ghcr.io/waddle-social/waddle/charts && rm -f waddle-server-*.tgz livekit-sfu-*.tgz"]
		}
		"gitops-push": schema.#Task & {
			command: "bash"
			args: ["-c", "flux push artifact oci://ghcr.io/waddle-social/waddle/gitops:latest --path=./gitops --source=$(git config --get remote.origin.url) --revision=$(git rev-parse --short HEAD)"]
		}
	}
}
