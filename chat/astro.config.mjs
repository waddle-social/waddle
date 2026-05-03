import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import tailwindcss from "@tailwindcss/vite";
import vue from "@astrojs/vue";
import { resolveCommitSha } from "./scripts/resolve-commit-sha.mjs";

const COMMIT_SHA = resolveCommitSha();

// Faro Web SDK config is baked at build time via Vite `define` so the
// bundled script knows where to POST beacons. Missing env vars degrade
// to an empty string, which `initTelemetry()` treats as "telemetry
// off" — no beacons leave the page. Set these in the Cloudflare Pages
// build environment (or via `wrangler pages deploy --var ...`). The
// URL is the "Send data" URL from the Grafana Cloud Frontend
// Observability app, e.g.
//   https://faro-collector-prod-eu-west-6.grafana.net/collect/<app-id>
const FARO_URL = process.env.PUBLIC_FARO_URL ?? "";
const FARO_APP_NAME = process.env.PUBLIC_FARO_APP_NAME ?? "waddle-chat";

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
      "import.meta.env.PUBLIC_FARO_URL": JSON.stringify(FARO_URL),
      "import.meta.env.PUBLIC_FARO_APP_NAME": JSON.stringify(FARO_APP_NAME),
    },
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("./src", import.meta.url)),
        events: "events",
      },
    },
    optimizeDeps: {
      include: ["events", "@waddle/xmpp-client-wasm"],
    },
  },

  integrations: [
    vue({
      appEntrypoint: "/src/vue-app.ts",
    }),
  ],
});
