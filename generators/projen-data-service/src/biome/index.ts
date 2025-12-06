import { Component, JsonFile, type Project } from "projen/lib/index.js";
import {
	type Configuration,
	IndentStyle,
	QuoteStyle,
	type RuleAssistPlainConfiguration,
	VcsClientKind,
	type VcsConfiguration,
} from "./types.ts";

type BiomeOptions = Partial<Configuration> & {
	vcs?: Partial<VcsConfiguration>;
};

export class Biome extends Component {
	constructor(project: Project, options: BiomeOptions = {}) {
		super(project);

		const defaultConfig: BiomeOptions = {
			$schema: "./node_modules/@biomejs/biome/configuration_schema.json",
			vcs: {
				enabled: true,
				clientKind: VcsClientKind.Git,
				useIgnoreFile: true,
			},
			formatter: {
				enabled: true,
				useEditorconfig: true,
				indentStyle: IndentStyle.Tab,
				indentWidth: 2,
			},
			linter: {
				enabled: true,
				rules: {
					recommended: true,
				},
			},
			javascript: {
				formatter: {
					quoteStyle: QuoteStyle.Double,
				},
			},
			assist: {
				enabled: true,
				actions: {
					source: {
						organizeImports: "on" satisfies RuleAssistPlainConfiguration,
					},
				},
			},
		};

		new JsonFile(project, "biome.json", {
			obj: {
				...defaultConfig,
				...options,
			},
		});
	}
}
