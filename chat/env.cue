package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	name: "waddle-chat"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		c.#FlakeHubCache,
		c.#CuenvRelease,
		c.#OnePassword,
		c.#BunWorkspace,
	]

	ci: provider: github: {
		flakehubCache: flakeName: "waddle-social/waddle"
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
			"tasks": [tasks.lint, tasks.build]
		}
	}

	tasks: {
		// Builds WASM from Rust source and publishes to GitHub Packages.
		// Runs on every merge to main.
		buildAndPublishWasm: schema.#Task & {
			command: "bun"
			args: ["run", "scripts/build-and-publish-wasm.mjs"]
			inputs: [
				"../server/Cargo.toml",
				"../server/Cargo.lock",
				"../server/crates/waddle-xmpp-client/**",
				"../server/crates/waddle-xmpp-client-wasm/**",
				"scripts/build-and-publish-wasm.mjs",
			]
		}

		// Builds WASM from Rust source for the PR lint/build pipelines.
		// Always rebuilds (REBUILD_WASM=1) so CI never needs committed artifacts.
		buildWasm: schema.#Task & {
			command: "bash"
			args: ["-c", "REBUILD_WASM=1 bun run wasm:build"]
			inputs: [
				"../server/Cargo.toml",
				"../server/Cargo.lock",
				"../server/crates/waddle-xmpp-client/**",
				"../server/crates/waddle-xmpp-client-wasm/**",
				"scripts/build-xmpp-wasm.mjs",
			]
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
				"../package.json",
				"../bun.lock",
				"package.json",
				"astro.config.mjs",
				"tsconfig.json",
				"wrangler.jsonc",
				"src/**",
				"public/**",
			]
			outputs: [
				"dist/**",
			]
		}

		preview: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "versions", "upload"]
			dependsOn: [build]
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
			dependsOn: [build]
			inputs: [
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
