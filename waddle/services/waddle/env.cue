package cuenv

import "github.com/cuenv/cuenv/schema"
import "github.com/waddle-social/waddle/cuenv:shared"

schema.#Cuenv
shared.#DataService

// I wonder if this will ever get larger than 5?
_forceCiCounter: 1

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
