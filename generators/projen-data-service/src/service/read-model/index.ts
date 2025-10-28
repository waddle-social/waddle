import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { Liquid } from "liquidjs";
import { SampleFile, TextFile, type Project, javascript } from "projen/lib/index.js";
import { Biome } from "../../biome/index.ts";
import { Bun } from "../../bun/index.ts";
import { TypeScriptConfig } from "../../tsconfig/index.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

interface Options {
  serviceName: string;
  databaseId: string;
  projectName: string;
  packageName: string;
  dataModelPackageName: string;
  dependencies: Record<string, string>;
  devDependencies: Record<string, string>;
}

export class ReadModel extends javascript.NodeProject {
  private readonly options: Options;
  private readonly liquid: Liquid;

  constructor(parent: Project, options: Options) {
    super({
      parent,
      outdir: "read-model",
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
    this.liquid = new Liquid({
      root: path.join(__dirname, "../../../templates/read-model"),
    });

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
    this.createWorkerEntry();
    this.createSchema();
    this.createSchemaWriter();
    this.createWranglerConfig();
    this.createViteConfig();
    this.createReadme();
  }

  public addRuntimeDependency(name: string, version: string) {
    this.addDeps(`${name}@${version}`);
  }

  public addRuntimeDependencies(deps: Record<string, string>) {
    for (const [name, version] of Object.entries(deps)) {
      this.addRuntimeDependency(name, version);
    }
  }

  public addDevelopmentDependency(name: string, version: string) {
    this.addDevDeps(`${name}@${version}`);
  }

  public addDevelopmentDependencies(deps: Record<string, string>) {
    for (const [name, version] of Object.entries(deps)) {
      this.addDevelopmentDependency(name, version);
    }
  }

  private configurePackage() {
    this.package.addVersion("0.0.0");
    this.package.addField("type", "module");
    this.package.addField("private", true);
    this.package.addField("exports", {
      ".": "./src/index.ts",
      "./schema": "./src/schema.ts",
      "./publish": "./publish.ts",
    });
    this.package.addField("files", [
      "src",
      "publish.ts",
      "vite.config.ts",
      "wrangler.jsonc",
    ]);

    const workspaceDependencies = {
      ...this.options.dependencies,
      [this.options.dataModelPackageName]: "workspace:*",
    };

    this.addRuntimeDependencies(workspaceDependencies);
    this.addDevelopmentDependencies(this.options.devDependencies);

    this.setScript("build", "vite build --config ./vite.config.ts");
    this.setScript("dev", "vite dev --config ./vite.config.ts");
    this.setScript(
      "preview",
      "bun run build && vite preview --config ./vite.config.ts",
    );
    this.setScript(
      "deploy",
      "bun run build && wrangler deploy --config ./wrangler.jsonc",
    );
    this.setScript("schema:publish", "bun run ./publish.ts");
  }

  private renderTemplate(relativePath: string) {
    return this.liquid.renderFileSync(relativePath, this.options);
  }

  private createWorkerEntry() {
    const content = this.renderTemplate("src/index.ts");

    new TextFile(this, "src/index.ts", {
      lines: content.split("\n"),
    });
  }

  private createSchema() {
    const content = this.renderTemplate("src/schema.ts");

    new SampleFile(this, "src/schema.ts", {
      contents: content,
    });
  }

  private createSchemaWriter() {
    const content = this.renderTemplate("publish.ts");

    new TextFile(this, "publish.ts", {
      lines: content.split("\n"),
    });
  }

  private createWranglerConfig() {
    const content = this.renderTemplate("wrangler.jsonc");

    new TextFile(this, "wrangler.jsonc", {
      lines: content.split("\n"),
    });
  }

  private createViteConfig() {
    const content = this.renderTemplate("vite.config.ts");

    new TextFile(this, "vite.config.ts", {
      lines: content.split("\n"),
    });
  }

  private createReadme() {
    const serviceTitle = this.formatServiceName(this.options.serviceName);

    const lines = [
      `# ${serviceTitle} Read Model`,
      "",
      "GraphQL read model powered by Cloudflare Workers and Vite.",
      "",
      "## Scripts",
      "",
      "- `bun run dev` – start the local worker with Vite.",
      "- `bun run build` – bundle the worker for production.",
      "- `bun run deploy` – deploy via Wrangler.",
      "- `bun run schema:publish` – emit the GraphQL schema to `schema.gql`.",
      "",
      "## Workspace Imports",
      "",
      `This package depends on \`${this.options.dataModelPackageName}\` for shared schema helpers.`,
      "",
      "```ts",
      `import * as dataSchema from "${this.options.dataModelPackageName}/schema";`,
      "```",
      "",
      "## Files",
      "",
      "- `src/index.ts` – Cloudflare Worker entry point.",
      "- `src/schema.ts` – Pothos GraphQL schema.",
      "- `publish.ts` – writes a federated schema snapshot.",
      "- `vite.config.ts` / `wrangler.jsonc` – tooling configuration.",
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
