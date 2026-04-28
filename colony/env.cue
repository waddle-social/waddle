package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

let _flakehubCacheContributor = schema.#Contributor & {
	id: "flakehubCache"
	tasks: [
		{
			id:       "nix.install"
			label:    "Install Determinate Nix"
			priority: 0
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
					"use-gha-cache":       "no-preference"
				}
			}
		},
	]
}

schema.#Project & {
	name: "waddle-colony"

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

	ci: pipelines: {
		default: {
			environment: "production"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			provider: github: permissions: "id-token": "write"
			"tasks": [tasks.deployMain]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: contents: "read"
			"tasks": [tasks.build]
		}
	}

	tasks: {
		build: schema.#Task & {
			command: "bun"
			args: ["run", "build"]
			inputs: [
				"../package.json",
				"../bun.lock",
				"package.json",
				"astro.config.ts",
				"tsconfig.json",
				"wrangler.jsonc",
				"drizzle.config.ts",
				"src/**",
				"public/**",
				"drizzle/**",
			]
			outputs: [
				"dist/**",
			]
		}
		dev: schema.#Task & {
			command: "bun"
			args: ["run", "dev"]
		}
		migrateProduction: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "d1", "migrations", "apply", "colony", "--remote"]
			dependsOn: [tasks.build]
			inputs: [
				"wrangler.jsonc",
				"drizzle/**",
			]
			outputs: [
				".wrangler/**",
			]
		}
		deployPreview: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "versions", "upload"]
			dependsOn: [tasks.build]
			captures: previewUrl: {
				pattern: "Version Preview URL: (.+)"
			}
			outputs: [
				".wrangler/**",
			]
		}
		deployProduction: schema.#Task & {
			command: "bun"
			args: ["x", "wrangler", "deploy"]
			dependsOn: [tasks.migrateProduction]
			inputs: [
				"wrangler.jsonc",
				"dist/**",
				"drizzle/**",
			]
			outputs: [
				".wrangler/**",
			]
		}
		deployMain: schema.#Task & {
			command: "true"
			dependsOn: [tasks.deployProduction]
		}
	}
}
