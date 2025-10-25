import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { Liquid } from "liquidjs";
import { Component, SampleFile, type Project, TextFile } from "projen";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

interface Options {
  serviceName: string;
  databaseId: string;
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

    // Create files during construction, not in synthesize
    this.createWorkerEntry();
    this.createSchema();
    this.createSchemaWriter();
    this.createWranglerConfig();
    this.createViteConfig();
  }

  private createWorkerEntry() {
    const content = this.liquid.renderFileSync("src/index.ts", this.options);

    new TextFile(this.project, "read-model/src/index.ts", {
      lines: content.split("\n"),
    });
  }

  private createSchema() {
    const content = this.liquid.renderFileSync("src/schema.ts", this.options);

    new SampleFile(this.project, "read-model/src/schema.ts", {
      contents: content,
    });
  }

  private createSchemaWriter() {
    const content = this.liquid.renderFileSync("publish.ts", this.options);

    new TextFile(this.project, "read-model/publish.ts", {
      lines: content.split("\n"),
    });
  }

  private createWranglerConfig() {
    const content = this.liquid.renderFileSync("wrangler.jsonc", this.options);

    new TextFile(this.project, "read-model/wrangler.jsonc", {
      lines: content.split("\n"),
    });
  }

  private createViteConfig() {
    const content = this.liquid.renderFileSync("vite.config.ts", this.options);

    new TextFile(this.project, "read-model/vite.config.ts", {
      lines: content.split("\n"),
    });
  }
}
