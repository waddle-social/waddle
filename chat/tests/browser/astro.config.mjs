import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import vue from "@astrojs/vue";

export default defineConfig({
  root: fileURLToPath(new URL("./fixture", import.meta.url)),
  srcDir: fileURLToPath(new URL("./fixture/src", import.meta.url)),
  integrations: [vue()],
  vite: {
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("../../src", import.meta.url)),
      },
    },
  },
});
