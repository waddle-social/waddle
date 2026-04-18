import { execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import tailwindcss from "@tailwindcss/vite";
import vue from "@astrojs/vue";

function resolveCommitSha() {
  const envSha = process.env.WADDLE_GIT_SHA ?? process.env.CF_PAGES_COMMIT_SHA;
  if (envSha && envSha.trim().length > 0) return envSha.trim().slice(0, 12);
  try {
    return execSync("git rev-parse --short=12 HEAD", { stdio: ["ignore", "pipe", "ignore"] })
      .toString()
      .trim();
  } catch {
    return "unknown";
  }
}

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
