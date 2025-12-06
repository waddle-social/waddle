import type { JSONSchemaForTheTypeScriptCompilerSConfigurationFile as Configuration } from "@schemastore/tsconfig";
import { Component, JsonFile, type Project } from "projen/lib/index.js";

export class TypeScriptConfig extends Component {
	constructor(project: Project, options: Configuration) {
		super(project);

		const defaultOptions: Configuration = {
			compilerOptions: {
				lib: ["ESNext"],
				target: "ESNext",
				module: "ESNext",
				moduleDetection: "force",
				jsx: "react-jsx",
				allowJs: true,
				moduleResolution: "bundler",
				verbatimModuleSyntax: true,
				noEmit: true,
				skipLibCheck: true,
				strict: true,
				noFallthroughCasesInSwitch: true,
				forceConsistentCasingInFileNames: true,
			},
			fileNames: [],
			errors: [],
		};

		new JsonFile(project, "tsconfig.json", {
			obj: {
				...defaultOptions,
				...options,
			},
		});
	}
}
