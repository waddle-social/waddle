package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	wc "github.com/waddle-social/waddle/ci/contributors"
)

schema.#Project & {
	name: "waddle-website"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		wc.#Nix,
		wc.#FlakeHubCache,
		wc.#Hestia,
		wc.#CuenvRelease,
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
			provider: github: permissions: {"id-token": "write", packages: "none"}
			"tasks": [tasks.deployMain]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			// Light pull-request lane: free GitHub-hosted runner (public repository).
			provider: github: runner: "ubuntu-24.04"
			provider: github: permissions: {
				contents:   "read"
				"id-token": "write"
				packages:   "none"
			}
			"tasks": [tasks.build]
		}
	}

	tasks: {
		generateTypes: schema.#Task & {
			command: "bun"
			args: ["run", "generate-types"]
			inputs: [
				schema.#ModuleFiles & {paths: ["package.json", "bun.lock"]},
				"package.json",
				"wrangler.jsonc",
			]
			outputs: [
				"worker-configuration.d.ts",
			]
		}
		dbGenerate: schema.#Task & {
			command: "bun"
			args: ["run", "db:generate"]
			inputs: [
				schema.#ModuleFiles & {paths: ["package.json", "bun.lock"]},
				"package.json",
				"drizzle.config.ts",
				"src/db/schema.ts",
			]
			outputs: [
				"drizzle/**",
			]
		}
		build: schema.#Task & {
			command: "bun"
			args: ["run", "build"]
			dependsOn: [tasks.generateTypes]
			inputs: [
				schema.#ModuleFiles & {paths: ["package.json", "bun.lock"]},
				"package.json",
				"astro.config.ts",
				"panda.config.ts",
				"tsconfig.json",
				"worker-configuration.d.ts",
				"wrangler.jsonc",
				"src/**",
				"public/**",
				schema.#ModuleFiles & {paths: ["design/package.json", "design/src/**", "design/assets/**"]},
			]
			outputs: [
				"dist/**",
			]
		}
		dev: schema.#Task & {
			command: "bun"
			args: ["run", "dev"]
		}
		migrateLocal: schema.#Task & {
			command: "bun"
			args: ["run", "db:migrate:local"]
			dependsOn: [tasks.dbGenerate]
			inputs: [
				"wrangler.jsonc",
				"drizzle/**",
			]
			outputs: [
				".wrangler/**",
			]
		}
		migrateProduction: schema.#Task & {
			command: "bun"
			args: ["run", "db:migrate:remote"]
			dependsOn: [tasks.build, tasks.dbGenerate]
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
			args: ["run", "deploy"]
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
