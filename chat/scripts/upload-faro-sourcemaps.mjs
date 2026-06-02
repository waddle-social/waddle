import { readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { create as createTar } from "tar";
import { resolveCommitSha } from "./resolve-commit-sha.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDir, "..");
const sourceMapRoot = resolve(projectRoot, "dist", "client");
const appName =
  readEnv("FARO_SOURCEMAP_APP_NAME")
  ?? readEnv("PUBLIC_FARO_APP_NAME")
  ?? "waddle-chat";
const commitSha = resolveCommitSha();
const bundleId = readEnv("FARO_BUNDLE_ID") ?? `${appName}-${commitSha}`;
ensureSourceMapRoot();

if (commitSha === "unknown" && !readEnv("FARO_BUNDLE_ID")) {
  fail("missing git commit SHA; set FARO_BUNDLE_ID or run from a Git checkout");
}

const endpoint = requireEnv("FARO_SOURCEMAP_ENDPOINT");
const appId = requireEnv("FARO_SOURCEMAP_APP_ID");
const stackId = requireEnv("FARO_SOURCEMAP_STACK_ID");
const apiKey = requireEnv("FARO_SOURCEMAP_API_KEY");
const verbose = readEnv("FARO_SOURCEMAP_VERBOSE") !== "false";
const sourceMaps = findFiles(sourceMapRoot, (name) => name.endsWith(".map"));

if (sourceMaps.length === 0) {
  fail(`no source maps found under ${relative(projectRoot, sourceMapRoot)}`);
}

await uploadSourceMapsArchive(endpoint, appId, stackId, apiKey, bundleId, sourceMaps);

for (const sourceMap of sourceMaps) {
  // Keep maps private: upload first, then remove them before Wrangler deploys dist.
  rmSync(sourceMap, { force: true });
}

console.log(
  `[faro] uploaded ${sourceMaps.length} source maps for ${appName} bundle ${bundleId}`,
);

function readEnv(name) {
  const value = process.env[name];
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function requireEnv(name) {
  const value = readEnv(name);
  if (!value) fail(`missing required environment variable ${name}`);
  return value;
}

function ensureSourceMapRoot() {
  try {
    if (statSync(sourceMapRoot).isDirectory()) return;
  } catch {
    // Handled below with the same message as a non-directory path.
  }
  fail("missing dist/client output; run bun run build before uploading source maps");
}

function findFiles(root, predicate) {
  const files = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) {
        visit(path);
      } else if (entry.isFile() && predicate(entry.name)) {
        files.push(path);
      }
    }
  };
  visit(root);
  return files.sort();
}

async function uploadSourceMapsArchive(uploadEndpoint, uploadAppId, uploadStackId, uploadApiKey, uploadBundleId, files) {
  const archivePath = resolve(sourceMapRoot, `faro-sourcemaps-${Date.now()}.tar.gz`);
  try {
    await createTar(
      {
        cwd: sourceMapRoot,
        file: archivePath,
        gzip: true,
      },
      files.map((file) => relative(sourceMapRoot, file)),
    );

    const url =
      `${uploadEndpoint.replace(/\/$/, "")}/app/${encodeURIComponent(uploadAppId)}/sourcemaps/${encodeURIComponent(uploadBundleId)}`;
    if (verbose) {
      console.log(`[faro] uploading ${files.length} source maps to ${url}`);
    }

    const response = await fetch(url, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${uploadStackId}:${uploadApiKey}`,
        "Content-Type": "application/gzip",
      },
      body: readFileSync(archivePath),
    });

    if (!response.ok) {
      const body = await response.text().catch(() => "");
      const details = body.trim().slice(0, 500);
      fail(
        `source-map upload failed with HTTP ${response.status}${details ? `: ${details}` : ""}`,
      );
    }
  } finally {
    rmSync(archivePath, { force: true });
  }
}

function fail(message) {
  console.error(`[faro] ${message}`);
  process.exit(1);
}
