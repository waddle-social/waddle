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
		}
		tasks: ["install", "build", "deploy"]
	},
	{
		name: "pull-request"
		when: pullRequest: true
		tasks: ["install", "build"]
	},
]

tasks: {
	install: {
		command: "bun"
		args: ["install"]
	}
	build: {
		command: "bun"
		args: ["run", "build"]
		dependsOn: ["install"]
	}
	dev: {
		command: "bun"
		args: ["run", "dev"]
		dependsOn: ["install"]
	}
	deploy: {
		command: "npx"
		args: ["wrangler", "deploy"]
		dependsOn: ["build"]
	}
}
