import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Component, type Project, SampleFile, TextFile } from "projen";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

export class DataModel extends Component {
	public readonly project: Project;

	constructor(project: Project) {
		super(project);

		this.project = project;

		this.createDrizzleConfig();
		this.createSchema();
		this.createZodSchemas();
	}

	private readTemplate(relativePath: string): string {
		const templatePath = path.join(
			__dirname,
			"../../../templates",
			relativePath,
		);
		return fs.readFileSync(templatePath, "utf-8");
	}

	private createDrizzleConfig() {
		const template = this.readTemplate("data-model/drizzle.config.ts");

		new TextFile(this.project, "drizzle.config.ts", {
			lines: template.split("\n"),
		});
	}

	private createSchema() {
		const template = this.readTemplate("data-model/schema.ts");

		new SampleFile(this.project, "data-model/schema.ts", {
			contents: template,
		});
	}

	private createZodSchemas() {
		const template = this.readTemplate("data-model/zod.ts");

		new SampleFile(this.project, "data-model/zod.ts", {
			contents: template,
		});
	}
}
