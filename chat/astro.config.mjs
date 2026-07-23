import { fileURLToPath } from "node:url";
import { format } from "node:util";
import { defineConfig } from "astro/config";
import cloudflare from "@astrojs/cloudflare";
import faroUploader from "@grafana/faro-rollup-plugin";
import tailwindcss from "@tailwindcss/vite";
import vue from "@astrojs/vue";
import { noiseSuppressionAudioWorkletVitePlugin } from "@workadventure/noise-suppression/vite";
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
const FARO_SOURCEMAP_ENDPOINT = process.env.FARO_SOURCEMAP_ENDPOINT ?? "";
const FARO_SOURCEMAP_APP_ID = process.env.FARO_SOURCEMAP_APP_ID ?? "";
const FARO_SOURCEMAP_STACK_ID = process.env.FARO_SOURCEMAP_STACK_ID ?? "";
const FARO_SOURCEMAP_API_KEY = process.env.FARO_SOURCEMAP_API_KEY ?? "";
const FARO_SOURCEMAP_VERBOSE = process.env.FARO_SOURCEMAP_VERBOSE !== "false";
const FARO_GIT_HASH = process.env.WADDLE_GIT_SHA ?? process.env.GITHUB_SHA ?? process.env.CF_PAGES_COMMIT_SHA;
const CLIENT_OUTPUT_SUFFIX = "/dist/client";
const FARO_CLIENT_SOURCE_MAP = /^_astro\/.*\.(?:js|mjs|cjs)\.map$/;
const FARO_UPLOAD_FAILURES = [/failed with status/i, /no sourcemaps uploaded/i];
const ANSI_PATTERN = /\u001b\[[0-9;]*m/g;

function faroSourcemapPlugins() {
  if (!FARO_SOURCEMAP_ENABLED) return [];

  const missing = [
    ["FARO_SOURCEMAP_ENDPOINT", FARO_SOURCEMAP_ENDPOINT],
    ["FARO_SOURCEMAP_APP_ID", FARO_SOURCEMAP_APP_ID],
    ["FARO_SOURCEMAP_STACK_ID", FARO_SOURCEMAP_STACK_ID],
    ["FARO_SOURCEMAP_API_KEY", FARO_SOURCEMAP_API_KEY],
  ].flatMap(([name, value]) => value ? [] : [name]);
  if (missing.length > 0) {
    throw new Error(`Faro source-map upload is enabled but missing ${missing.join(", ")}`);
  }

  const plugin = faroUploader({
    appName: FARO_APP_NAME,
    endpoint: FARO_SOURCEMAP_ENDPOINT,
    apiKey: FARO_SOURCEMAP_API_KEY,
    appId: FARO_SOURCEMAP_APP_ID,
    stackId: FARO_SOURCEMAP_STACK_ID,
    bundleId: FARO_BUNDLE_ID,
    gitHash: FARO_GIT_HASH,
    gzipContents: true,
    keepSourcemaps: false,
    outputFiles: FARO_CLIENT_SOURCE_MAP,
    prefixPath: "_astro/",
    prefixPathBasenameOnly: true,
    verbose: FARO_SOURCEMAP_VERBOSE,
  });

  return [clientOutputOnly(failOnFaroUploadFailure(plugin))];
}

function failOnFaroUploadFailure(plugin) {
  return {
    ...plugin,
    async writeBundle(outputOptions, bundle) {
      const sourceMaps = Object.keys(bundle).filter((fileName) => FARO_CLIENT_SOURCE_MAP.test(fileName));
      if (sourceMaps.length === 0) {
        throw new Error("Faro source-map upload is enabled but no client JavaScript source maps were generated");
      }

      const captured = captureFaroConsole();
      try {
        await plugin.writeBundle?.call(this, outputOptions, bundle);
      } finally {
        captured.restore();
      }

      const failure = captured.messages.find(({ level, message }) =>
        level === "error" || FARO_UPLOAD_FAILURES.some((pattern) => pattern.test(message)));
      if (failure) {
        throw new Error(`Faro source-map upload failed: ${failure.message}`);
      }
    },
  };
}

function captureFaroConsole() {
  const originalInfo = console.info;
  const originalError = console.error;
  const messages = [];
  const capture = (level, original) => (...args) => {
    messages.push({ level, message: format(...args).replace(ANSI_PATTERN, "") });
    return original.apply(console, args);
  };

  console.info = capture("info", originalInfo);
  console.error = capture("error", originalError);

  return {
    messages,
    restore() {
      console.info = originalInfo;
      console.error = originalError;
    },
  };
}

function clientOutputOnly(plugin) {
  return {
    ...plugin,
    renderChunk(code, chunk, outputOptions) {
      if (!isClientOutput(outputOptions)) return null;
      return plugin.renderChunk?.call(this, code, chunk, outputOptions) ?? null;
    },
    writeBundle(outputOptions, bundle) {
      if (!isClientOutput(outputOptions)) return undefined;
      return plugin.writeBundle?.call(this, outputOptions, bundle);
    },
  };
}

function isClientOutput(outputOptions) {
  const outputDir = outputOptions.dir?.replaceAll("\\", "/") ?? "";
  return outputDir.endsWith(CLIENT_OUTPUT_SUFFIX);
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
        // The `events` polyfill (npm package, see `resolve.alias`
        // below) is only meaningful in the browser bundle, so its
        // pre-bundling is scoped to the client environment. The
        // Cloudflare worker (SSR) environment marks Node's `events`
        // as external because workerd provides `node:events` natively;
        // esbuild 0.27+ now hard-errors when an `optimizeDeps` entry
        // point is also externalized, which is exactly what happened
        // when this `include` lived at the top level and leaked into
        // the worker environment.
        optimizeDeps: {
          include: ["events"],
        },
      },
    },
    plugins: [
      tailwindcss(),
      // Serve the DTLN (@workadventure/noise-suppression) AudioWorklet module
      // and its bundled LiteRT/model assets from our own origin — the #914 AI
      // noise filter must be self-hosted, no third-party CDN.
      noiseSuppressionAudioWorkletVitePlugin(),
      ...faroSourcemapPlugins(),
      // Force `Cache-Control: no-store` on every URL containing the WASM
      // package name. Vite's `?v=<optimizer-hash>` URL convention sends
      // `Cache-Control: max-age=31536000, immutable` to the browser, and the
      // optimizer hash is deterministic — it does NOT change after a
      // fresh WASM rebuild because the package content is excluded from the
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
      // Exclude the local WASM package from esbuild pre-bundling. Pre-bundling
      // produces a stable v= cache-buster hash derived from the lock file rather
      // than file mtime, so the browser never re-fetches after a WASM rebuild.
      // Serving the package directly lets Vite use mtime-based cache busting so
      // a fresh WASM build is always picked up without a hard refresh.
      exclude: ["@waddle/xmpp-client-wasm"],
    },
    server: {
      watch: {
        // Vite's default chokidar config ignores `**/node_modules/**`. The
        // local WASM package lives there (installed via `file:` and bun's
        // .bun/ cache symlink), so without this Vite never observes a
        // fresh rewrite of `_bg.js` / `_bg.wasm`. The dev server
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
