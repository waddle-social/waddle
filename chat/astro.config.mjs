import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import tailwindcss from "@tailwindcss/vite";
import vue from "@astrojs/vue";
import { resolveCommitSha } from "./scripts/resolve-commit-sha.mjs";

const COMMIT_SHA = resolveCommitSha();

export default defineConfig({
  output: "server",
  adapter: cloudflare(),

  server: {
    port: 4321,
  },

  vite: {
    plugins: [tailwindcss()],
    define: {
      "import.meta.env.PUBLIC_COMMIT_SHA": JSON.stringify(COMMIT_SHA),
    },
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
