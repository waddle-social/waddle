package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

let _flakehubCacheContributor = schema.#Contributor & {
	id: "flakehubCache"
	tasks: [
		{
			id:       "namespace.nixCache"
			label:    "Set up Namespace Nix cache"
			priority: -1
			provider: github: {
				uses: "namespacelabs/nscloud-cache-action@v1"
				with: cache: "nix"
			}
		},
		{
			id:       "nix.install"
			label:    "Install Determinate Nix"
			priority: 0
			dependsOn: ["namespace.nixCache"]
			provider: github: {
				uses: "DeterminateSystems/determinate-nix-action@92ffb5400c3776307a27a1727d7e2ac3dcd9f844"
				with: "extra-conf": "accept-flake-config = true"
			}
		},
		{
			id:       "flakehubCache.setup"
			label:    "Setup FlakeHub Cache"
			priority: 9
			dependsOn: ["nix.install"]
			provider: github: {
				uses: "DeterminateSystems/flakehub-cache-action@e134de896b2302c1584a7b54ff35432708607d44"
				with: {
					"flakehub-flake-name": "waddle-social/waddle"
					"use-gha-cache":       "disabled"
				}
			}
		},
	]
}

schema.#Project & {
	name: "waddle-chat"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		_flakehubCacheContributor,
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
			dependsOn: [generateTypes]
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
			dependsOn: [generateTypes]
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
