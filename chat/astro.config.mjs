import { fileURLToPath } from "node:url";
import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import MagicString from "magic-string";
import tailwindcss from "@tailwindcss/vite";
import vue from "@astrojs/vue";
import { resolveCommitSha } from "./scripts/resolve-commit-sha.mjs";

const COMMIT_SHA = resolveCommitSha();
// Faro Web SDK config is baked at build time via Vite `define`. The
// PUBLIC_FARO_* values are injected only for production deploys (see
// env.environment.production in the repo-root env.cue); every other
// build leaves them empty, which initTelemetry() treats as "off".
const FARO_URL = process.env.PUBLIC_FARO_URL ?? "";
const FARO_APP_NAME = process.env.PUBLIC_FARO_APP_NAME ?? "waddle-chat";
const FARO_APP_VERSION = process.env.PUBLIC_FARO_APP_VERSION ?? "1.0.0";
const FARO_ENVIRONMENT = process.env.PUBLIC_FARO_ENVIRONMENT ?? "";
const FARO_SOURCEMAP_ENABLED = process.env.FARO_SOURCEMAP_ENABLED === "true";
const FARO_BUNDLE_ID = process.env.FARO_BUNDLE_ID ?? `${FARO_APP_NAME}-${COMMIT_SHA}`;

function faroBundleIdPlugin() {
  return {
    name: "waddle-faro-bundle-id",
    apply: "build",
    renderChunk(code, chunk, outputOptions) {
      const outputDir = outputOptions.dir?.replaceAll("\\", "/") ?? "";
      if (
        !FARO_SOURCEMAP_ENABLED
        || !outputDir.endsWith("/dist/client")
        || !/\.(?:js|mjs|cjs)$/.test(chunk.fileName)
        || code.includes(`__faroBundleId_${FARO_APP_NAME}`)
      ) {
        return null;
      }

      const snippet =
        `(function(){try{var g=typeof window!=="undefined"?window:typeof global!=="undefined"?global:typeof self!=="undefined"?self:{};g[${JSON.stringify(`__faroBundleId_${FARO_APP_NAME}`)}]=${JSON.stringify(FARO_BUNDLE_ID)}}catch(l){}})();\n`;
      const source = new MagicString(code);
      source.prepend(snippet);
      return {
        code: source.toString(),
        map: source.generateMap({ hires: true }),
      };
    },
  };
}

export default defineConfig({
  output: "server",
  adapter: cloudflare(),

  server: {
    port: 4321,
  },

  vite: {
    environments: {
      client: {
        build: {
          sourcemap: FARO_SOURCEMAP_ENABLED,
        },
      },
    },
    plugins: [
      tailwindcss(),
      faroBundleIdPlugin(),
      // Force `Cache-Control: no-store` on every URL containing the WASM
      // package name. Vite's `?v=<optimizer-hash>` URL convention sends
      // `Cache-Control: max-age=31536000, immutable` to the browser, and the
      // optimizer hash is deterministic — it does NOT change after a
      // REBUILD_WASM=1 because the package content is excluded from the
      // optimizer (see `optimizeDeps.exclude` below). Result: identical URL,
      // year-long immutable cache, browser keeps serving the previous build's
      // glue JS while the new .wasm has different closure indices →
      // "wasm.__wasm_bindgen_func_elem_N is not a function".
      // Wrapping `res.setHeader` lets us override Vite's later writes too,
      // which a normal middleware running before the static-file handler
      // cannot.
      {
        name: "waddle-wasm-disable-cache",
        apply: "serve",
        configureServer(server) {
          server.middlewares.use((req, res, next) => {
            if (req.url && req.url.includes("xmpp-client-wasm")) {
              const setHeader = res.setHeader.bind(res);
              res.setHeader = (name, value) =>
                name.toLowerCase() === "cache-control"
                  ? setHeader(name, "no-store, must-revalidate")
                  : setHeader(name, value);
              setHeader("Cache-Control", "no-store, must-revalidate");
            }
            next();
          });
        },
      },
    ],
    define: {
      "import.meta.env.PUBLIC_COMMIT_SHA": JSON.stringify(COMMIT_SHA),
      "import.meta.env.PUBLIC_FARO_URL": JSON.stringify(FARO_URL),
      "import.meta.env.PUBLIC_FARO_APP_NAME": JSON.stringify(FARO_APP_NAME),
      "import.meta.env.PUBLIC_FARO_APP_VERSION": JSON.stringify(FARO_APP_VERSION),
      "import.meta.env.PUBLIC_FARO_ENVIRONMENT": JSON.stringify(FARO_ENVIRONMENT),
    },
    resolve: {
      alias: {
        "@": fileURLToPath(new URL("./src", import.meta.url)),
        events: "events",
      },
    },
    optimizeDeps: {
      include: ["events"],
      // Exclude the local WASM package from esbuild pre-bundling. Pre-bundling
      // produces a stable v= cache-buster hash derived from the lock file rather
      // than file mtime, so the browser never re-fetches after a WASM rebuild.
      // Serving the package directly lets Vite use mtime-based cache busting so
      // a fresh REBUILD_WASM=1 build is always picked up without a hard refresh.
      exclude: ["@waddle/xmpp-client-wasm"],
    },
    server: {
      watch: {
        // Vite's default chokidar config ignores `**/node_modules/**`. The
        // local WASM package lives there (installed via `file:` and bun's
        // .bun/ cache symlink), so without this Vite never observes a
        // REBUILD_WASM=1 rewrite of `_bg.js` / `_bg.wasm`. The dev server
        // keeps serving the cached transform of the *old* `_bg.js` while
        // the browser fetches a *fresh* `_bg.wasm` (cache: "no-store"),
        // producing "wasm.__wasm_bindgen_func_elem_N is not a function".
        // Negation patterns in `ignored` are honoured by anymatch — when a
        // path matches both a positive ignore and a negation, the negation
        // wins, so this whitelists our package without touching the rest of
        // node_modules. The pattern matches both the symlink path and bun's
        // resolved .bun/ cache path.
        ignored: ["!**/node_modules/@waddle/xmpp-client-wasm/**"],
      },
    },
  },

  integrations: [
    vue({
      appEntrypoint: "/src/vue-app.ts",
    }),
  ],
});
