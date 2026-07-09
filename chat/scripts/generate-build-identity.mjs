import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  BUILD_IDENTITY_FILE_NAME,
  resolveBuildIdentity,
  serializeBuildIdentity,
} from "./build-identity.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));

export function generateBuildIdentityArtifact(options = {}) {
  const projectRoot = options.projectRoot ?? resolve(scriptDir, "..");
  const identity = options.identity ?? resolveBuildIdentity(options);
  const outputPath = resolve(projectRoot, "public", BUILD_IDENTITY_FILE_NAME);
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, serializeBuildIdentity(identity));
  return { identity, outputPath };
}

if (import.meta.main) generateBuildIdentityArtifact();
