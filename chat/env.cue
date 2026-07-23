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

// These source inputs can change the generated wasm-bindgen glue or WebAssembly
// binary. The three crate paths are the complete local dependency graph of the
// browser binding crate.
let _WasmSourceInputs = [
	"../.cargo/**",
	"../flake.lock",
	"../flake.nix",
	"../server/.cargo/**",
	"../server/Cargo.lock",
	"../server/Cargo.toml",
	"../server/crates/waddle-xmpp-client-wasm/**",
	"../server/crates/waddle-xmpp-client/**",
	"../server/crates/waddle-xmpp-core/**",
	"../server/rust-toolchain.toml",
]

// The local UI builder also depends on its invocation and package contract.
// The task-level sentinel below handles cuenv's cross-project affected match.
let _WasmBuildInputs = list.Concat([_WasmSourceInputs, [
	"env.cue",
	"package.json",
	"scripts/build-xmpp-wasm.mjs",
]])

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
			"tasks": [tasks.test, tasks.lint, tasks.build, tasks.tokensCheck]
		}
	}

	tasks: {
		// cuenv 0.54 generates correct repository-relative workflow filters for
		// ../server inputs but normalizes those paths differently during runtime
		// affected-task matching. A task without path inputs is always affected;
		// its dependency edge keeps a triggered WASM workflow from being skipped.
		wasmPipelineTrigger: schema.#Task & {
			command: "true"
		}

		// Builds WASM from Rust source and publishes to GitHub Packages.
		// Runs on every merge to main.
		buildAndPublishWasm: schema.#Task & {
			command: "bun"
			args: ["run", "scripts/build-and-publish-wasm.mjs"]
			dependsOn: [wasmPipelineTrigger]
			inputs: list.Concat([_WasmSourceInputs, [
				"env.cue",
				"scripts/build-and-publish-wasm.mjs",
			]])
		}

		// Builds WASM from Rust source for the PR lint/build pipelines.
		// The script always rebuilds so CI never needs committed artifacts.
		buildWasm: schema.#Task & {
			command: "bun"
			args: ["run", "wasm:build"]
			dependsOn: [wasmPipelineTrigger]
			inputs: _WasmBuildInputs
			outputs: [
				"../server/wasm-pkg/waddle-xmpp-client-wasm/**",
			]
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
			args: ["run", "knip"]
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
			command: "bash"
			args: ["-c", "bun run generate-service-worker && bun run copy-background-assets && ./node_modules/.bin/astro check && ./node_modules/.bin/astro build"]
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
