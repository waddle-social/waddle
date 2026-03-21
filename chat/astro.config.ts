import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import vue from "@astrojs/vue";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  output: "server",
  adapter: cloudflare(),
  integrations: [vue()],
  server: {
    port: 4321,
  },
  vite: {
    plugins: [tailwindcss()],
  },
});
