import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { Liquid } from "liquidjs";
import { JsonFile, Project, TextFile } from "projen";
import { Biome } from "./biome/index.ts";
import { Bun } from "./bun/index.ts";
import type { WaddleDataServiceOptions } from "./options.ts";
import { DataModel } from "./service/data-model/index.ts";
import { ReadModel } from "./service/read-model/index.ts";
import { WriteModel } from "./service/write-model/index.ts";
import { TypeScriptConfig } from "./tsconfig/index.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

export class WaddleDataService extends Project {
	private readonly options: Omit<
		Required<WaddleDataServiceOptions>,
		"bindings"
	> & {
		bindings?: WaddleDataServiceOptions["bindings"];
	};
	private readonly dependencies: Record<string, string>;
	private readonly devDependencies: Record<string, string>;

	constructor(options: WaddleDataServiceOptions) {
		super({
			name: options.serviceName,
			outdir: ".",
			commitGenerated: false,
		});

		this.tasks.tryFind("default")?.reset("bun run .projenrc.ts");

		this.options = {
			includeWriteModel: false,
			additionalDependencies: {},
			additionalDevDependencies: {},
			...options,
		};

		// Validation: D1 database with binding "DB" required
		const hasDbBinding = this.options.bindings?.d1Databases?.some(
			(db) => db.binding === "DB",
		);

		if (!hasDbBinding) {
			throw new Error(
				'bindings.d1Databases must include a database with binding "DB"',
			);
		}

		// Initialize dependencies
		this.dependencies = {
			"@paralleldrive/cuid2": "^2.2.2",
			"@sindresorhus/slugify": "^2.2.1",
			"drizzle-orm": "^0.38.4",
			"drizzle-zod": "^0.6.1",
			zod: "^3.24.3",
			// GraphQL dependencies for read model
			"@apollo/subgraph": "^2.10.2",
			"@graphql-tools/utils": "^10.8.6",
			"@pothos/core": "^4.6.0",
			"@pothos/plugin-directives": "^4.2.0",
			"@pothos/plugin-drizzle": "^0.8.1",
			"@pothos/plugin-federation": "^4.3.2",
			graphql: "^16.10.0",
			"graphql-scalars": "^1.24.2",
			"graphql-yoga": "^5.13.4",
		};

		// Add additional dependencies last to allow overrides
		Object.assign(this.dependencies, this.options.additionalDependencies);

		this.devDependencies = {
			"@biomejs/biome": "^1.9.4",
			"@cloudflare/vite-plugin": "^1.13.15",
			"@cloudflare/workers-types": "^4.20250426.0",
			"@types/bun": "latest",
			"@types/node": "^22.15.2",
			"drizzle-kit": "^0.30.6",
			vite: "^7.1.12",
			vitest: "^1.2.17",
			wrangler: "^4.45.0",
			...this.options.additionalDevDependencies,
		};

		// Create components
		new DataModel(this);

		new ReadModel(this, {
			serviceName: this.options.serviceName,
			bindings: this.options.bindings,
		});

		if (this.options.includeWriteModel) {
			new WriteModel(this, {
				workflows: [],
				bindings: this.options.bindings,
			});
		}

		this.createReadme();
		this.createPackageJson();
		this.createConfigFiles();
	}

	/**
	 * Register additional dependencies
	 */
	public addDependency(name: string, version: string): void {
		this.dependencies[name] = version;
	}

	/**
	 * Register multiple dependencies at once
	 */
	public addDependencies(deps: Record<string, string>): void {
		Object.assign(this.dependencies, deps);
	}

	/**
	 * Register additional dev dependencies
	 */
	public addDevDependency(name: string, version: string): void {
		this.devDependencies[name] = version;
	}

	/**
	 * Register multiple dev dependencies at once
	 */
	public addDevDependencies(deps: Record<string, string>): void {
		Object.assign(this.devDependencies, deps);
	}

	private createPackageJson() {
		// Sort dependencies alphabetically
		const sortedDeps = Object.keys(this.dependencies)
			.sort()
			.reduce(
				(acc, key) => {
					acc[key] = this.dependencies[key];
					return acc;
				},
				{} as Record<string, string>,
			);

		const sortedDevDeps = Object.keys(this.devDependencies)
			.sort()
			.reduce(
				(acc, key) => {
					acc[key] = this.devDependencies[key];
					return acc;
				},
				{} as Record<string, string>,
			);

		new JsonFile(this, "package.json", {
			obj: {
				name: this.options.serviceName,
				private: true,
				type: "module",
				scripts: {
					lint: "biome check .",
					tsc: "tsc --noEmit",
					test: "vitest run",
				},
				dependencies: sortedDeps,
				devDependencies: sortedDevDeps,
			},
		});
	}

	private createConfigFiles() {
		new TypeScriptConfig(this, {});
		new Biome(this);
		new Bun(this);
	}

	private createReadme() {
		const templatesDir = path.join(__dirname, "../templates");

		const liquid = new Liquid({
			root: templatesDir,
			extname: ".md",
		});

		liquid.registerFilter("camelCase", (str: string) => this.toCamelCase(str));
		liquid.registerFilter("pascalCase", (str: string) =>
			this.toPascalCase(str),
		);

		const context = {
			serviceName: this.options.serviceName,
			serviceNameCamel: this.toCamelCase(this.options.serviceName),
			serviceNamePascal: this.toPascalCase(this.options.serviceName),
			includeWriteModel: this.options.includeWriteModel,
		};

		const readmeContent = liquid.renderFileSync("README", context);

		new TextFile(this, "README.md", {
			lines: readmeContent.split("\n"),
		});
	}

	private toCamelCase(str: string): string {
		return str
			.split("-")
			.map((word, index) =>
				index === 0 ? word : word.charAt(0).toUpperCase() + word.slice(1),
			)
			.join("");
	}

	private toPascalCase(str: string): string {
		return str
			.split("-")
			.map((word) => word.charAt(0).toUpperCase() + word.slice(1))
			.join("");
	}
}
