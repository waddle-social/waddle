import { Component, SampleFile, type Project, TextFile } from "projen";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

export class DataModel extends Component {
  public readonly project: Project;

  constructor(project: Project) {
    super(project);

    this.project = project;
    this.createSchema();
    this.createDrizzleConfig();
    this.createMigrationsDirectory();
  }

  private readTemplate(relativePath: string) {
    const templatePath = path.join(
      __dirname,
      "../../../templates",
      relativePath,
    );
    return fs.readFileSync(templatePath, "utf-8");
  }

  private createDrizzleConfig() {
    const template = this.readTemplate("data-model/drizzle.config.ts");

    new TextFile(this.project, "data-model/drizzle.config.ts", {
      lines: template.split("\n"),
    });
  }

  private createSchema() {
    const template = this.readTemplate("data-model/schema.ts");

    new SampleFile(this.project, "data-model/schema.ts", {
      contents: template,
    });
  }

  private createMigrationsDirectory() {
    new TextFile(this.project, "data-model/migrations/.gitkeep", {
      lines: [],
    });
  }
}
