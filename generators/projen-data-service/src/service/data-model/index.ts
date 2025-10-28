import { SampleFile, TextFile, type Project, javascript } from "projen";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Biome } from "../../biome/index.ts";
import { Bun } from "../../bun/index.ts";
import { TypeScriptConfig } from "../../tsconfig/index.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

export interface DataModelOptions {
  serviceName: string;
  projectName: string;
  packageName: string;
  version?: string;
  dependencies: Record<string, string>;
  devDependencies: Record<string, string>;
}

export class DataModel extends javascript.NodeProject {
  private readonly options: DataModelOptions;

  constructor(parent: Project, options: DataModelOptions) {
    super({
      parent,
      outdir: "data-model",
      name: options.projectName,
      packageName: options.packageName,
      defaultReleaseBranch: "main",
      packageManager: javascript.NodePackageManager.BUN,
      entrypoint: "",
      github: false,
      release: false,
      depsUpgrade: false,
      jest: false,
      eslint: false,
      prettier: false,
      projenrcTs: false,
      projenrcJs: false,
      projenrcJson: false,
      sampleCode: false,
      buildWorkflow: false,
      pullRequestTemplate: false,
    });

    this.options = options;

    this.package.installTask.reset();
    this.package.installTask.exec("true");
    this.package.installCiTask.reset();
    this.package.installCiTask.exec("true");

    this.package.addField("license", "UNLICENSED");
    this.tryRemoveFile("LICENSE");

    new TypeScriptConfig(this, {});
    new Biome(this);
    new Bun(this);

    this.configurePackage();
    this.createSchema();
    this.createZodSchemas();
    this.createDrizzleConfig();
    this.createReadme();
  }

  private configurePackage() {
    const version = this.options.version ?? "0.0.0";

    this.package.addVersion(version);
    this.package.addField("type", "module");
    this.package.addField("private", true);
    this.package.addField("exports", {
      "./schema": "./schema.ts",
      "./zod": "./zod.ts",
      "./drizzle.config": "./drizzle.config.ts",
      "./migrations/*": "./migrations/*",
    });
    this.package.addField("files", [
      "schema.ts",
      "zod.ts",
      "drizzle.config.ts",
      "migrations",
    ]);

    for (const [dep, versionRange] of Object.entries(this.options.dependencies)) {
      this.addDeps(`${dep}@${versionRange}`);
    }

    for (const [dep, versionRange] of Object.entries(
      this.options.devDependencies,
    )) {
      this.addDevDeps(`${dep}@${versionRange}`);
    }
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

    new TextFile(this, "drizzle.config.ts", {
      lines: template.split("\n"),
    });
  }

  private createSchema() {
    const template = this.readTemplate("data-model/schema.ts");

    new SampleFile(this, "schema.ts", {
      contents: template,
    });
  }

  private createZodSchemas() {
    const template = this.readTemplate("data-model/zod.ts");

    new SampleFile(this, "zod.ts", {
      contents: template,
    });
  }

  private createReadme() {
    const serviceTitle = this.formatServiceName(this.options.serviceName);

    const lines = [
      `# ${serviceTitle} Data Model`,
      "",
      "Shared Drizzle schema and validation helpers for the data service.",
      "",
      "## Included",
      "",
      "- `schema.ts` – Drizzle ORM table definitions.",
      "- `zod.ts` – Zod schemas derived from the Drizzle definitions.",
      "- `drizzle.config.ts` – Configuration used by Drizzle Kit when generating migrations.",
      "- `migrations/` – Generated migration files.",
      "",
      "## Usage",
      "",
      "Import the schema or helpers from the workspace package:",
      "",
      "```ts",
      `import * as schema from "${this.options.packageName}/schema";`,
      `import { WADDLE_VISIBILITY_VALUES } from "${this.options.packageName}/zod";`,
      "```",
    ];

    new TextFile(this, "README.md", {
      lines,
    });
  }

  private formatServiceName(name: string) {
    return name
      .split("-")
      .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
      .join(" ");
  }
}
