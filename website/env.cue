package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	name: "waddle-website"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		c.#Nix,
		c.#CuenvRelease,
		c.#OnePassword,
		c.#BunWorkspace,
	]

	ci: pipelines: {
		default: {
			environment: "production"
			when: {
				branch:        ["main"]
				defaultBranch: true
				manual:        true
			}
			"tasks": [tasks.deployMain]
		}
		pullRequest: {
			environment: "production"
			when: {
				pullRequest: true
			}
			"tasks": [tasks.deployPreview]
			annotations: "Preview URL": schema.#TaskCaptureRef & {
				cuenvTask:    "deployPreview"
				cuenvCapture: "previewUrl"
			}
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
		dbGenerate: schema.#Task & {
			command: "bun"
			args: ["run", "db:generate"]
			inputs: [
				"../package.json",
				"../bun.lock",
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
				"../package.json",
				"../bun.lock",
				"package.json",
				"astro.config.ts",
				"tsconfig.json",
				"worker-configuration.d.ts",
				"wrangler.jsonc",
				"src/**",
				"public/**",
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
