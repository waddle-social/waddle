package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	xNix "github.com/cuenv/cuenv/contrib/nix"
)

schema.#Base & {
	config: ci: cuenv: {
		source:  "nix"
		version: "self"
	}

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

		"cuenv-skills": {
			url:       "https://github.com/cuenv/cuenv.git"
			reference: "main"
			vendor:    false
			subdir:    ".agents/skills"
			path:      ".agents/skills"
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
			PUBLIC_FARO_APP_NAME:    "waddle-chat"
			PUBLIC_FARO_ENVIRONMENT: "production"

			FARO_SOURCEMAP_ENDPOINT: "https://faro-api-prod-eu-west-6.grafana.net/faro/api/v1"
			FARO_SOURCEMAP_APP_ID:   "92"
			FARO_SOURCEMAP_APP_NAME: "waddle-chat"
			FARO_SOURCEMAP_ENABLED:  "true"
			FARO_SOURCEMAP_STACK_ID: "1602000"
			FARO_SOURCEMAP_API_KEY: schema.#OnePasswordRef & {
				ref: "op://waddle-production/Grafana-Faro/source-maps"
			}
		}
	}
}
