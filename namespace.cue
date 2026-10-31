package namespace

import "github.com/cuenv/cuenv/schema"

let _serverRustInputs = [
	"flake.lock",
	"flake.nix",
	"server/Cargo.toml",
	"server/Cargo.lock",
	"server/README.md",
	"server/capabilities.toml",
	"server/.cargo/**",
	"server/.config/nextest.toml",
	"server/rust-toolchain.toml",
	"server/crates/**",
	"server/extensions/**",
	"server/vendor/**",
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
				// Every Mimir rule file, so a second rules file is visible to
				// the rule-drift tests in this sandbox too.
				from: "target/cuenv/server-rust-inputs/rules/mimir"
				to:   "infrastructure/waddle.cloud/rules/mimir"
			},
		]
	},
]

let _nativeRustEnv = {
	PKG_CONFIG_PATH: schema.#EnvPassthrough
}

schema.#Project & {
	name: "waddle-server-namespace-rust-checks"
	runtime: {type: "nix", flake: "."}

	tasks: {
		// The no-Postgres, default-feature lane: no database is provisioned,
		// so Postgres-backed tests return early, and `clustering` tests are
		// not compiled. Postgres and all-features coverage is the shard lane
		// (server `nixTestLocal`), which also gates publish.
		test: schema.#Task & {
			command: "cargo"
			args: ["nextest", "run", "--workspace", "--all-targets", "--locked", "--profile", "ci"]
			dir: {from: "module", path: "server"}
			inputs: _serverRustInputs
			env:    _nativeRustEnv
			hermetic: sandbox: "dir"
		}

		clippy: schema.#Task & {
			command: "bash"
			args: ["-euo", "pipefail", "-c", "python3 scripts/check-poison-recovery.py && python3 scripts/check-rust-structure.py --server-only && python3 scripts/check-test-postgres-env.py && exec cargo clippy --workspace --all-targets --all-features --locked -- -D warnings"]
			dir: {from: "module", path: "server"}
			inputs: _serverRustInputs
			env:    _nativeRustEnv
			hermetic: sandbox: "dir"
		}

		doctest: schema.#Task & {
			command: "cargo"
			args: ["test", "--doc", "--workspace", "--all-features", "--locked"]
			dir: {from: "module", path: "server"}
			inputs: _serverRustInputs
			env:    _nativeRustEnv
			hermetic: sandbox: "dir"
		}
	}
}
