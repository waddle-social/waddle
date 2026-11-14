import { defineConfig } from "@pandacss/dev";
import { waddlePreset } from "@waddle/design";

export default defineConfig({
  presets: ["@pandacss/preset-base", waddlePreset],
  preflight: true,
  include: ["./src/**/*.{ts,astro}"],
  outdir: "styled-system",
  globalCss: {
    a: { color: "action.text", _hover: { color: "fg" } },
  },
});
