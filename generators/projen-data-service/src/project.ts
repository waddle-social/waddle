import { Liquid } from "liquidjs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { Project, TextFile } from "projen/lib/index.js";
import type { WaddleDataServiceOptions } from "./options.ts";
import { DataModel } from "./service/data-model/index.ts";
import { ReadModel } from "./service/read-model/index.ts";
import { WriteModel } from "./service/write-model/index.ts";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

export class WaddleDataService extends Project {
  private readonly options: Required<WaddleDataServiceOptions>;
  private readonly dataModelProject: DataModel;
  private readonly readModelProject: ReadModel;

  constructor(options: WaddleDataServiceOptions) {
    super({
      name: options.serviceName,
      outdir: ".",
      commitGenerated: false,
      gitIgnoreOptions: {
        ignorePatterns: ["data-model/", "read-model/", "write-model/"],
      },
    });

    this.tasks.tryFind("default")?.reset("bun run .projenrc.ts");

    this.options = {
      includeWriteModel: false,
      additionalDependencies: {},
      additionalDevDependencies: {},
      ...options,
    };

    const dataModelDependencies = {
      "drizzle-orm": "^0.38.4",
      "drizzle-zod": "^0.6.1",
      zod: "^3.24.3",
    };

    const dataModelDevDependencies = {
      "drizzle-kit": "^0.30.6",
    };

    const readModelDependencies = {
      "@apollo/subgraph": "^2.10.2",
      "@graphql-tools/utils": "^10.8.6",
      "@paralleldrive/cuid2": "^2.2.2",
      "@pothos/core": "^4.6.0",
      "@pothos/plugin-directives": "^4.2.0",
      "@pothos/plugin-drizzle": "^0.8.1",
      "@pothos/plugin-federation": "^4.3.2",
      "@sindresorhus/slugify": "^2.2.1",
      "drizzle-orm": "^0.38.4",
      graphql: "^16.10.0",
      "graphql-scalars": "^1.24.2",
      "graphql-yoga": "^5.13.4",
    };

    const readModelDevDependencies = {
      "@biomejs/biome": "^1.9.4",
      "@cloudflare/vite-plugin": "^1.13.15",
      "@cloudflare/workers-types": "^4.20250426.0",
      "@types/bun": "latest",
      "@types/node": "^22.15.2",
      vite: "^7.1.12",
      vitest: "^1.2.17",
      wrangler: "^4.45.0",
    };

    this.dataModelProject = new DataModel(this, {
      serviceName: this.options.serviceName,
      projectName: this.getDataModelProjectName(),
      packageName: this.getDataModelPackageName(),
      dependencies: dataModelDependencies,
      devDependencies: dataModelDevDependencies,
    });

    this.readModelProject = new ReadModel(this, {
      serviceName: this.options.serviceName,
      databaseId: this.options.databaseId,
      projectName: this.getReadModelProjectName(),
      packageName: this.getReadModelPackageName(),
      dataModelPackageName: this.getDataModelPackageName(),
      dependencies: readModelDependencies,
      devDependencies: readModelDevDependencies,
    });

    this.readModelProject.addRuntimeDependencies(
      this.options.additionalDependencies,
    );
    this.readModelProject.addDevelopmentDependencies(
      this.options.additionalDevDependencies,
    );

    if (this.options.includeWriteModel) {
      new WriteModel(this, {
        databaseId: this.options.databaseId,
        workflows: [],
      });
    }

    this.createReadme();
  }

  /**
   * Register additional dependencies
   */
  public addDependency(name: string, version: string): void {
    this.readModelProject.addRuntimeDependency(name, version);
  }

  /**
   * Register multiple dependencies at once
   */
  public addDependencies(deps: Record<string, string>): void {
    this.readModelProject.addRuntimeDependencies(deps);
  }

  /**
   * Register additional dev dependencies
   */
  public addDevDependency(name: string, version: string): void {
    this.readModelProject.addDevelopmentDependency(name, version);
  }

  /**
   * Register multiple dev dependencies at once
   */
  public addDevDependencies(deps: Record<string, string>): void {
    this.readModelProject.addDevelopmentDependencies(deps);
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

  private getDataModelPackageName(): string {
    const normalized = this.getNormalizedServiceName();

    return `@waddlesocial/waddle-service-${normalized}-data-model`;
  }

  private getReadModelPackageName(): string {
    const normalized = this.getNormalizedServiceName();

    return `@waddlesocial/waddle-service-${normalized}-read-model`;
  }

  private getDataModelProjectName(): string {
    return `${this.getNormalizedServiceName()}-data-model`;
  }

  private getReadModelProjectName(): string {
    return `${this.getNormalizedServiceName()}-read-model`;
  }

  private getNormalizedServiceName(): string {
    return this.options.serviceName
      .toLowerCase()
      .replace(/[^a-z0-9-]/g, "-");
  }
}
