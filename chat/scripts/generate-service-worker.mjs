import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { resolveCommitSha } from "./resolve-commit-sha.mjs";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDir, "..");
const templatePath = resolve(projectRoot, "src", "service-worker", "sw-template.js");
const outputPath = resolve(projectRoot, "public", "sw.js");

const commitSha = resolveCommitSha();
const template = readFileSync(templatePath, "utf8");
const output = template.replaceAll("__WADDLE_BUILD_SHA__", commitSha);

mkdirSync(dirname(outputPath), { recursive: true });
writeFileSync(outputPath, output);
