package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "waddle-colony-website"

let _t = tasks

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

ci: pipelines: {
	default: {
		environment: "production"
		when: {
			branch:        ["main"]
			defaultBranch: true
		}
		tasks: [_t.deploy]
	}
	pullRequest: {
		environment: "production"
		when: {
			pullRequest: true
		}
		tasks: [_t.build]
	}
}

tasks: {
	install: {
		command: "bun"
		args: ["install"]
	}
	build: {
		command: "bun"
		args: ["run", "build"]
		dependsOn: [_t.install]
	}
	dev: {
		command: "bun"
		args: ["run", "dev"]
		dependsOn: [_t.install]
	}
	deploy: {
		command: "bun"
		args: ["x", "wrangler", "deploy"]
		dependsOn: [_t.build]
	}
}
