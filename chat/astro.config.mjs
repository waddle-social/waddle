import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import tailwindcss from "@tailwindcss/vite";
import vue from "@astrojs/vue";

export default defineConfig({
  output: "server",
  adapter: cloudflare(),

  server: {
    port: 4321,
  },

  vite: {
    plugins: [tailwindcss()],
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("./src", import.meta.url)),
        events: "events",
      },
    },
    optimizeDeps: {
      include: ["events", "stanza"],
    },
  },

  integrations: [vue()],
});
