import { defineConfig } from "@pandacss/dev";
import { waddlePreset } from "@waddle/design";

export default defineConfig({
	presets: ["@pandacss/preset-base", waddlePreset],
	preflight: true,
	include: ["./src/**/*.{ts,astro}"],
	exclude: ["./src/**/*.test.ts"],
	outdir: "styled-system",
});
