package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	xNix "github.com/cuenv/cuenv/contrib/nix"
)

schema.#Base & {
	runtime: type: "nix"

	hooks: onEnter: nix: xNix.#NixFlake

	formatters: rust: {
		edition: "2021"
		includes: [
			"server/**/*.rs",
		]
	}

	vcs: {
		xeps: {
			url:       "https://github.com/xsf/xeps.git"
			reference: "master"
			vendor:    false
			path:      "xeps"
		}
	}

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
}
