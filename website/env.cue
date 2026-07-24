package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
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

schema.#Project & {
	name: "waddle-website"

	runtime: {
		type:  "nix"
		flake: ".."
	}

	ci: providers: ["github"]
	ci: contributors: [
		_NamespaceNix,
		c.#CuenvNix,
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
			"tasks": [tasks.deployMain]
		}
		pullRequest: {
			when: {
				pullRequest: true
			}
			provider: github: permissions: {
				contents:   "read"
				"id-token": "write"
			}
			"tasks": [tasks.build]
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
