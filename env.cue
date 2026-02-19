package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	xBun "github.com/cuenv/cuenv/contrib/bun"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Base

runtime: schema.#ToolsRuntime & {
	platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64", "linux-arm64"]
	tools: {
		bun: xBun.#Bun & {version: "1.3.9"}
	}
}

hooks: onEnter: tools: schema.#ToolsActivate

ci: providers: ["github"]
ci: contributors: [
	c.#Nix,
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
