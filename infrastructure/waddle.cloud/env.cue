package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "waddle-cloud"

	tasks: {
		"helm-push": {
			command: "bash"
			args: ["-c", "helm package ../../server/charts/waddle-server && helm push waddle-server-*.tgz oci://ghcr.io/waddle-social/waddle/charts && rm -f waddle-server-*.tgz"]
		}
		"gitops-push": {
			command: "bash"
			args: ["-c", "flux push artifact oci://ghcr.io/waddle-social/waddle/gitops:latest --path=./gitops --source=$(git config --get remote.origin.url) --revision=$(git rev-parse --short HEAD)"]
		}
	}
}
