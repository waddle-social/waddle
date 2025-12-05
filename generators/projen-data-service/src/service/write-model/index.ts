import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { Liquid } from "liquidjs";
import { Component, type Project, TextFile } from "projen";
import type { CloudflareBindings } from "../../options.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

export interface WorkflowDefinition {
	name: string;
	binding: string;
	className: string;
	scriptName: string;
}

interface Options {
	workflows: WorkflowDefinition[];
	bindings?: CloudflareBindings;
}

export class WriteModel extends Component {
	public readonly project: Project;
	private options: Options;
	private liquid: Liquid;

	constructor(project: Project, options: Options) {
		super(project);

		this.project = project;
		this.options = options;

		this.liquid = new Liquid({
			root: path.join(__dirname, "../../../templates/write-model"),
		});

		this.createWranglerConfig();
	}

	private getTemplateContext() {
		const bindings = this.options.bindings ?? {};

		// Ensure DB binding has migrations_dir for write-model
		const d1Databases = (bindings.d1Databases ?? []).map((db) => {
			if (db.binding === "DB" && !db.migrations_dir) {
				return { ...db, migrations_dir: "../data-model/migrations" };
			}
			return db;
		});

		return {
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
			workflows: this.options.workflows,
		};
	}

	private createWranglerConfig() {
		const content = this.liquid.renderFileSync(
			"wrangler.jsonc",
			this.getTemplateContext(),
		);

		new TextFile(this.project, "write-model/wrangler.jsonc", {
			lines: content.split("\n"),
		});
	}
}
