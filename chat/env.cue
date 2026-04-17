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
		c.#Nix,
		c.#CuenvRelease,
		c.#OnePassword,
	]

	ci: pipelines: {
		default: {
			environment: "production"
			when: {
				branch: ["main"]
				defaultBranch: true
				manual:        true
			}
			"tasks": [tasks.deploy]
		}
		pullRequest: {
			environment: "production"
			when: {
				pullRequest: true
			}
			"tasks": [tasks.preview]
			annotations: "Preview URL": schema.#TaskCaptureRef & {
				cuenvTask:    "preview"
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
