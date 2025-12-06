package shared

#DataService: {
	tasks: {
		install: {
			command: "bun"
			args: ["install"]
		}
		lint: {
			command: "bun"
			args: ["run", "lint"]
			dependsOn: ["install"]
		}
		tsc: {
			command: "bun"
			args: ["run", "tsc"]
			dependsOn: ["install"]
		}
		test: {
			command: "bun"
			args: ["run", "test"]
			dependsOn: ["install"]
		}
		deploy: {
			command: "bunx"
			args: ["wrangler", "deploy", "--config", "./read-model/wrangler.jsonc"]
			dependsOn: ["install"]
		}
	}
	ci: pipelines: [
		{
			name: "default"
			when: {
				branch: ["main"]
				defaultBranch: true
			}
			tasks: ["install", "test", "deploy"]
		},
		{
			name: "pull-request"
			when: pullRequest: true
			tasks: ["install", "test"]
		},
	]
}
