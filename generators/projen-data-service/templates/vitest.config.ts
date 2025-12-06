import { defineConfig } from "vitest/config";

export default defineConfig({
	test: {
		globals: true,
		environment: "node",
		include: ["**/*.test.ts"],
		passWithNoTests: true,
		coverage: {
			reporter: ["text", "lcov"],
			include: ["**/src/**/*.ts"],
		},
	},
});
