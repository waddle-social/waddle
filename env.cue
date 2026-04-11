package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	xBun "github.com/cuenv/cuenv/contrib/bun"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

name: "waddle"

let _s = services

services: {
	server: {
		dir:         "server"
		description: "Waddle XMPP server"
		script: """
			set -a
			. ./local/waddle.env
			set +a
			cargo run --bin waddle-server
			"""
		readiness: {
			kind: "port"
			port: 5222
		}
		logs: color: "magenta"
	}
	chat: {
		dir:         "chat"
		command:     "bun"
		args: ["x", "wrangler", "dev", "--local", "--port", "4321"]
		description: "Astro chat frontend (Wrangler local dev)"
		dependsOn: [_s.server]
		readiness: {
			kind: "port"
			port: 4321
		}
		watch: {
			paths: ["src/**", "public/**"]
			ignore: ["node_modules/**", "dist/**"]
		}
		logs: color: "cyan"
	}
}

runtime: schema.#ToolsRuntime & {
	platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64", "linux-arm64"]
	tools: {
		bun: xBun.#Bun & {version: "1.3.10"}
	}
}

hooks: onEnter: tools: schema.#ToolsActivate

ci: providers: ["github"]
ci: contributors: [
	c.#CuenvRelease,
	c.#OnePassword,
]

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
