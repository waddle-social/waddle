package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	name: "waddle-colony"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		c.#Nix,
		c.#CuenvRelease,
		c.#OnePassword,
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
