import { defineConfig } from "astro/config";

import cloudflare from "@astrojs/cloudflare";
import panda from "@pandacss/vite";
import { waddleFonts } from "@waddle/design/astro-fonts";

// https://astro.build/config
export default defineConfig({
  compressHTML: true,
  site: "https://waddle.social",
  output: "server",

  devToolbar: {
    enabled: false,
  },

  vite: {
    plugins: [panda()],
  },

  fonts: waddleFonts,

  adapter: cloudflare(),
});
