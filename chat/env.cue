package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	wc "github.com/waddle-social/waddle/ci/contributors"
	"list"
)

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

let _NativeTelemetryContractInputs = [
	"env.cue",
	"package.json",
	"tests/native-telemetry-off.test.ts",
	"../apps/android/**/build.gradle.kts",
	"../apps/android/gradle/libs.versions.toml",
	"../apps/android/settings.gradle.kts",
	"../scripts/check-native-remote-telemetry.sh",
	"../server/crates/waddle-xmpp-client/**",
	"../server/crates/waddle-xmpp-client-ffi/**",
	"../server/crates/waddle-xmpp-core/**",
	"../server/crates/waddle-xmpp-client-wasm/Cargo.toml",
	"../server/crates/waddle-xmpp-client-wasm/src/**",
	"../server/wasm-pkg/waddle-xmpp-client-wasm/**",
]

schema.#Project & {
	name: "waddle-chat"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#Hestia,
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
		default: {
			environment: "production"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: {
				"id-token":      "write"
				packages:        "none"
				"pull-requests": "none"
			}
			"tasks": [tasks.nativeTelemetryContract, tasks.deploy]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: {
				contents:   "read"
				"id-token": "write"
				packages:   "none"
			}
			"tasks": [tasks.nativeTelemetryContract, tasks.test, tasks.lint, tasks.build, tasks.tokensCheck]
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

		nativeTelemetryContract: schema.#Task & {
			command: "bun"
			args: ["test", "tests/native-telemetry-off.test.ts"]
			dependsOn: [buildWasm, generateTypes]
			inputs: _NativeTelemetryContractInputs
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
				"../scripts/check-native-remote-telemetry.sh",
				"../server/crates/waddle-xmpp-client-wasm/Cargo.toml",
				"../server/crates/waddle-xmpp-client-wasm/src/**",
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
