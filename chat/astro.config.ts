import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  output: "server",
  adapter: cloudflare(),
  server: {
    port: 4321,
  },
  vite: {
    plugins: [tailwindcss()],
  },
});
