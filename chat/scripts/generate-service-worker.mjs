import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveBuildIdentity } from "./build-identity.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
export function generateServiceWorker(options = {}) {
  const projectRoot = options.projectRoot ?? resolve(scriptDir, "..");
  const templatePath = resolve(projectRoot, "src", "service-worker", "sw-template.js");
  const outputPath = resolve(projectRoot, "public", "sw.js");
  const identity = options.identity ?? resolveBuildIdentity(options);
  const template = readFileSync(templatePath, "utf8");
  if (!template.includes("__WADDLE_BUILD_SHA__")) {
    throw new Error("service-worker template is missing the build SHA placeholder");
  }
  const output = template.replaceAll("__WADDLE_BUILD_SHA__", identity.commitSha);

  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, output);
  return { identity, outputPath };
}

if (import.meta.main) generateServiceWorker();
