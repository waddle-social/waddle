import { defineConfig } from "@pandacss/dev";
import { waddlePreset } from "@waddle/design";

export default defineConfig({
	presets: ["@pandacss/preset-base", waddlePreset],
	preflight: true,
	include: ["./src/**/*.{ts,astro}"],
	exclude: ["./src/**/*.test.ts"],
	outdir: "styled-system",
	theme: {
		extend: {
			keyframes: {
				spin: { to: { transform: "rotate(360deg)" } },
				moonBob: { "0%, 100%": { transform: "translateY(0)" }, "50%": { transform: "translateY(-6px)" } },
			},
		},
	},
});
