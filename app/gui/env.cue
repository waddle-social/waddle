package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {
	environment: production: {
		CLOUDFLARE_ACCOUNT_ID: schema.#OnePasswordRef & {
			ref: "op://waddle-production/Cloudflare/username"
		}
		CLOUDFLARE_API_TOKEN: schema.#OnePasswordRef & {
			ref: "op://waddle-production/Cloudflare/password"
		}
	}
}

ci: pipelines: [
	{
		name: "default"
		when: {
			branch:        ["main"]
			defaultBranch: true
			manual:        true
		}
		tasks: ["install", "build", "deploy-production"]
	},
	{
		name: "pull-request"
		when: pullRequest: true
		tasks: ["install", "build", "deploy-preview"]
	},
]

tasks: {
	install: {
		command: "bun"
		args: ["install", "--frozen-lockfile"]
	}
	build: {
		command: "bun"
		args: ["run", "build"]
		dependsOn: ["install"]
	}
	"deploy-production": {
		command: "bun"
		args: ["x", "wrangler", "deploy", "--config", "wrangler.jsonc", "--env", "production"]
		dependsOn: ["build"]
	}
	"deploy-preview": {
		command: "sh"
		args: [
			"-c",
			"bun x wrangler deploy --config wrangler.jsonc --name waddle-gui-pr-${PR_NUMBER:?PR_NUMBER is required}",
		]
		dependsOn: ["build"]
	}
	"delete-preview": {
		command: "sh"
		args: [
			"-c",
			"bun x wrangler delete --config wrangler.jsonc --name waddle-gui-pr-${PR_NUMBER:?PR_NUMBER is required} --force",
		]
	}
}
