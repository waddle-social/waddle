import { defineConfig } from "vitest/config";

export default defineConfig({
	test: {
		globals: true,
		environment: "node",
		include: ["read-model/tests/**/*.test.ts", "data-model/tests/**/*.test.ts"],
		passWithNoTests: true,
		coverage: {
			reporter: ["text", "lcov"],
			include: ["**/src/**/*.ts"],
		},
	},
	resolve: {
		alias: {
			"~": new URL("./read-model", import.meta.url).pathname,
		},
	},
});
