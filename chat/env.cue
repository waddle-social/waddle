package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	"list"
)

let _NamespaceNix = schema.#Contributor & {
	id: "namespaceNix"
	when: runtimeType: ["nix"]
	tasks: [
		{
			id:       "nix.cache"
			label:    "Cache /nix on Namespace volume"
			priority: 0
			provider: github: {
				uses: "namespacelabs/nscloud-cache-action@v1"
				with: cache: "nix"
			}
		},
		{
			id:       "nix.chown"
			label:    "Hand /nix to the runner user"
			priority: 1
			dependsOn: ["nix.cache"]
			script: "sudo chown -R runner /nix"
		},
		{
			id:       "nix.install"
			label:    "Install Nix"
			priority: 2
			dependsOn: ["nix.chown"]
			provider: github: {
				uses: "cachix/install-nix-action@v31"
				with: extra_nix_config: "accept-flake-config = true"
			}
		},
	]
}

// Keep generated path filters aligned with the versioned manifest consumed by
// the WASM builder. env.cue and package.json are triggers because they own the
// task/package invocation; only the relevant contract descriptor is hashed.
let _WasmBuildInputs = [
	"../flake.lock",
	"../flake.nix",
	"../server/.cargo/**",
	"../server/Cargo.lock",
	"../server/Cargo.toml",
	"../server/crates/waddle-xmpp-client-wasm/Cargo.toml",
	"../server/crates/waddle-xmpp-client-wasm/build.rs",
	"../server/crates/waddle-xmpp-client-wasm/src/**",
	"../server/crates/waddle-xmpp-client/Cargo.toml",
	"../server/crates/waddle-xmpp-client/build.rs",
	"../server/crates/waddle-xmpp-client/src/**",
	"../server/crates/waddle-xmpp-core/Cargo.toml",
	"../server/crates/waddle-xmpp-core/build.rs",
	"../server/crates/waddle-xmpp-core/src/**",
	"../server/rust-toolchain.toml",
	"env.cue",
	"package.json",
	"scripts/build-and-publish-wasm.mjs",
	"scripts/build-xmpp-wasm.mjs",
	"scripts/wasm-build-contract.mjs",
	"scripts/wasm-build-contract.json",
	"scripts/wasm-build-environment.mjs",
	"scripts/wasm-build-executor.mjs",
	"scripts/wasm-build-input-digest.mjs",
	"scripts/wasm-build-input-manifest.mjs",
	"scripts/wasm-build-inputs.mjs",
	"scripts/wasm-build-module-closure.mjs",
	"scripts/wasm-package-bindings.mjs",
	"scripts/wasm-rust-source-closure.mjs",
]

schema.#Project & {
	name: "waddle-chat"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		_NamespaceNix,
		c.#CuenvRelease,
		c.#OnePassword,
		c.#BunWorkspace,
	]

	ci: provider: github: {
		runner: "namespace-profile-linux-x86"
		runners: arch: {
			"linux-x64":    "namespace-profile-linux-x86"
			"darwin-arm64": "namespace-profile-darwin-arm64"
			amd64:          "namespace-profile-linux-x86"
		}
	}

	ci: pipelines: {
		publishWasm: {
			when: {
				branch: ["main"]
				defaultBranch: true
			}
			provider: github: permissions: {
				contents: "read"
				packages: "write"
			}
			"tasks": [tasks.buildAndPublishWasm]
		}
		default: {
			environment: "production"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: "id-token": "write"
			"tasks": [tasks.deploy]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: {
				contents:   "read"
				"id-token": "write"
			}
			"tasks": [tasks.test, tasks.lint, tasks.build, tasks.tokensCheck, tasks.checkWasmDrift]
		}
	}

	tasks: {
		// Builds WASM from Rust source and publishes to GitHub Packages.
		// Runs on every merge to main.
		buildAndPublishWasm: schema.#Task & {
			command: "bun"
			args: ["run", "scripts/build-and-publish-wasm.mjs"]
			inputs: _WasmBuildInputs
		}

		// Builds WASM from Rust source for the PR lint/build pipelines.
		// Always rebuilds (REBUILD_WASM=1) so CI never needs committed artifacts.
		buildWasm: schema.#Task & {
			command: "bash"
			args: ["-c", "REBUILD_WASM=1 bun run wasm:build"]
			dependsOn: [checkWasmDrift]
			inputs: _WasmBuildInputs
			outputs: [
				"../server/wasm-pkg/waddle-xmpp-client-wasm/**",
			]
		}

		// Rebuilds into an isolated temporary directory and compares only the
		// tracked bindings. It never repairs drift or mutates generated outputs.
		checkWasmDrift: schema.#Task & {
			command: "bun"
			args: ["run", "scripts/build-xmpp-wasm.mjs", "--check"]
			inputs: list.Concat([_WasmBuildInputs, [
				"../server/wasm-pkg/waddle-xmpp-client-wasm/package.json",
				"../server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.d.ts",
				"../server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js",
			]])
		}

		generateTypes: schema.#Task & {
			command: "bun"
			args: ["run", "generate-types"]
			inputs: [
				"../package.json",
				"../bun.lock",
				"package.json",
				"wrangler.jsonc",
			]
			outputs: [
				"worker-configuration.d.ts",
			]
		}

		lint: schema.#Task & {
			command: "bun"
			args: ["run", "lint"]
			dependsOn: [buildWasm, generateTypes]
			inputs: [
				"../package.json",
				"../bun.lock",
				"package.json",
				"knip.json",
				"tsconfig.json",
				"astro.config.mjs",
				"src/**",
				"scripts/**",
				"tests/**",
			]
		}

		// Fails when generated brand-palette outputs drift from tokens.css.
		tokensCheck: schema.#Task & {
			command: "bun"
			args: ["../scripts/generate-design-tokens.mjs", "--check"]
			inputs: [
				"../scripts/generate-design-tokens.mjs",
				"src/styles/global/tokens.css",
				"../website/src/styles/global/brand-palette.css",
				"../apps/apple/Waddle/Assets.xcassets/AccentColor.colorset/Contents.json",
			]
		}

		test: schema.#Task & {
			command: "bun"
			args: ["test"]
			dependsOn: [build]
			inputs: [
				"../package.json",
				"../bun.lock",
				"package.json",
				"tsconfig.json",
				"src/**",
				"tests/**",
				"dist/**",
			]
		}

		dev: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "dev", "--local", "--port", "4321"]
			dependsOn: [build]
		}

		build: schema.#Task & {
			command: "bun"
			args: ["run", "build"]
			dependsOn: [buildWasm, generateTypes]
			inputs: [
				"../env.cue",
				"../package.json",
				"../bun.lock",
				"package.json",
				"astro.config.mjs",
				"scripts/generate-service-worker.mjs",
				"scripts/resolve-commit-sha.mjs",
				"tsconfig.json",
				"wrangler.jsonc",
				"src/**",
				"public/**",
			]
			outputs: [
				"dist/**",
			]
		}

		stripSourcemaps: schema.#Task & {
			command: "bun"
			args: ["run", "sourcemaps:strip"]
			dependsOn: [build]
			inputs: [
				"package.json",
				"scripts/strip-sourcemaps.mjs",
				"dist/**",
			]
			outputs: [
				"dist/**",
			]
		}

		preview: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "versions", "upload"]
			dependsOn: [stripSourcemaps]
			captures: previewUrl: {
				pattern: "Version Preview URL: (.+)"
			}
			outputs: [
				".wrangler/**",
			]
		}

		deploy: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "deploy"]
			dependsOn: [stripSourcemaps]
			inputs: [
				"../env.cue",
				"wrangler.jsonc",
				"dist/**",
			]
			outputs: [
				".wrangler/**",
			]
		}
	}

	services: chat: {
		dir: "chat"
		entrypoint: schema.#Command & {
			command: "bun"
			args: ["x", "wrangler", "dev", "--local", "--port", "4321"]
		}
		description: "Astro chat frontend (Wrangler local dev)"
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
