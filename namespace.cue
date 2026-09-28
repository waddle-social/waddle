package namespace

import "github.com/cuenv/cuenv/schema"

let _serverRustInputs = [
	"server/Cargo.toml",
	"server/Cargo.lock",
	"server/README.md",
	"server/capabilities.toml",
	"server/.cargo/**",
	"server/.config/nextest.toml",
	"server/rust-toolchain.toml",
	"server/crates/**",
	"server/extensions/**",
	"server/scripts/**",
	"server/wit/**",
	"server/charts/waddle-server/**",
	{
		project: "infrastructure/waddle.cloud"
		task:    "namespaceServerRustInputs"
		map: [
			{
				from: "target/cuenv/server-rust-inputs/gitops/waddle-server/postgresql-monitoring-ingress.yaml"
				to:   "infrastructure/waddle.cloud/gitops/waddle-server/postgresql-monitoring-ingress.yaml"
			},
			{
				from: "target/cuenv/server-rust-inputs/rules/mimir/waddle-reliability.yaml"
				to:   "infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml"
			},
		]
	},
]

schema.#Project & {
	name: "waddle-server-namespace-rust-checks"
	runtime: {type: "nix", flake: "."}

	tasks: {
		test: schema.#Task & {
			command: "cargo"
			args: ["nextest", "run", "--workspace", "--all-targets", "--locked", "--profile", "ci"]
			dir: {from: "module", path: "server"}
			inputs: _serverRustInputs
			hermetic: sandbox: "dir"
		}

		clippy: schema.#Task & {
			command: "cargo"
			args: ["clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]
			dir: {from: "module", path: "server"}
			inputs: _serverRustInputs
			hermetic: sandbox: "dir"
		}

		doctest: schema.#Task & {
			command: "cargo"
			args: ["test", "--doc", "--workspace", "--all-features", "--locked"]
			dir: {from: "module", path: "server"}
			inputs: _serverRustInputs
			hermetic: sandbox: "dir"
		}
	}
}
