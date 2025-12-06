import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { Liquid } from "liquidjs";
import { Component, type Project, SampleFile, TextFile } from "projen";
import type { CloudflareBindings } from "../../options.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

interface Options {
	serviceName: string;
	bindings?: CloudflareBindings;
}

export class ReadModel extends Component {
	public readonly project: Project;
	private options: Options;
	private liquid: Liquid;

	constructor(project: Project, options: Options) {
		super(project);

		this.project = project;
		this.options = options;

		this.liquid = new Liquid({
			root: path.join(__dirname, "../../../templates/read-model"),
		});

		// Create files during construction
		this.createWorkerEntry();
		this.createSchema();
		this.createSchemaWriter();
		this.createWranglerConfig();
		this.createViteConfig();

		// Ignore generated schema file (output by publish.ts)
		project.addGitIgnore("/read-model/schema.gql");
		// Ignore wrangler local state
		project.addGitIgnore("/read-model/.wrangler/");
	}

	private getTemplateContext() {
		const bindings = this.options.bindings ?? {};

		// Ensure DB binding has migrations_dir for read-model
		const d1Databases = (bindings.d1Databases ?? []).map((db) => {
			if (db.binding === "DB" && !db.migrations_dir) {
				return { ...db, migrations_dir: "../data-model/migrations" };
			}
			return db;
		});

		return {
			serviceName: this.options.serviceName,
			bindings: {
				d1Databases,
				secretStoreSecrets: bindings.secretStoreSecrets ?? [],
				kvNamespaces: bindings.kvNamespaces ?? [],
				r2Buckets: bindings.r2Buckets ?? [],
				services: bindings.services ?? [],
				workflows: bindings.workflows ?? [],
				sendEmail: bindings.sendEmail ?? [],
				ai: bindings.ai,
				vars: bindings.vars ?? {},
				crons: bindings.crons ?? [],
				routes: bindings.routes ?? [],
			},
		};
	}

	private createWorkerEntry() {
		const content = this.liquid.renderFileSync(
			"src/index.ts",
			this.getTemplateContext(),
		);

		new TextFile(this.project, "read-model/src/index.ts", {
			lines: content.split("\n"),
		});
	}

	private createSchema() {
		const content = this.liquid.renderFileSync(
			"src/schema.ts",
			this.getTemplateContext(),
		);

		new SampleFile(this.project, "read-model/src/schema.ts", {
			contents: content,
		});
	}

	private createSchemaWriter() {
		const content = this.liquid.renderFileSync(
			"publish.ts",
			this.getTemplateContext(),
		);

		new TextFile(this.project, "read-model/publish.ts", {
			lines: content.split("\n"),
		});
	}

	private createWranglerConfig() {
		const content = this.liquid.renderFileSync(
			"wrangler.jsonc",
			this.getTemplateContext(),
		);

		new TextFile(this.project, "read-model/wrangler.jsonc", {
			lines: content.split("\n"),
		});
	}

	private createViteConfig() {
		const content = this.liquid.renderFileSync(
			"vite.config.ts",
			this.getTemplateContext(),
		);

		new TextFile(this.project, "read-model/vite.config.ts", {
			lines: content.split("\n"),
		});
	}
}
