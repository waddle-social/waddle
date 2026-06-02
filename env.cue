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

			// Public Grafana Cloud Faro collector for chat RUM. cuenv scrubs
			// ambient env, so build-time PUBLIC_* values must be declared here
			// to reach `astro build`. Scoped to production so previews, PR, and
			// local builds ship telemetry off.
			PUBLIC_FARO_URL:         "https://faro-collector-prod-eu-west-6.grafana.net/collect/0eab89b00ec9f7cfd5c97e96636a3d20"
			PUBLIC_FARO_ENVIRONMENT: "production"
		}
	}
}
