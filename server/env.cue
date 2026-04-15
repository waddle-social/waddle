package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	name: "waddle-server"

	ci: providers: ["github"]
	ci: contributors: [
		c.#CuenvRelease,
		c.#OnePassword,
	]

	tasks: {
		build: {
			command: "cargo"
			args: ["build", "--bin", "waddle-server"]
			inputs: [
				"Cargo.toml",
				"Cargo.lock",
				"crates/**",
			]
		}

		dev: {
			command: "cargo"
			args: [
				"run",
				"--bin",
				"waddle-server",
			]
			dependsOn: [build]
		}
	}

	env: environment: test: {
		WADDLE_CERTS_EPHEMERAL:            "true"
		WADDLE_TEST_FIXED_ACCOUNT_ENABLED: "true"
	}

	services: {
		server: {
			description: "Waddle XMPP server"
			command:     "cargo"
			args: ["run", "--bin", "waddle-server"]
			readiness: {
				kind: "port"
				port: 5222
			}
			logs: color: "magenta"
		}
	}
}
