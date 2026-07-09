import { rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const deploymentEnvironment = {
  ...process.env,
  REBUILD_WASM: "1",
  WADDLE_REQUIRE_IMMUTABLE_BUILD: "true",
  WADDLE_REQUIRE_FARO_BUILD: "true",
};

function run(command) {
  const result = Bun.spawnSync(command, {
    cwd: projectRoot,
    env: deploymentEnvironment,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  if (result.exitCode !== 0) process.exit(result.exitCode ?? 1);
}

// A production deploy must derive every uploaded file from this invocation.
// In particular, never allow `cuenv task deploy -S` to reuse ignored output
// from an earlier build or a Wrangler-generated implicit configuration.
rmSync(resolve(projectRoot, "dist"), { recursive: true, force: true });
rmSync(resolve(projectRoot, ".wrangler"), { recursive: true, force: true });
run(["bun", "run", "generate-types"]);
run(["bun", "run", "build"]);
run(["bun", "run", "sourcemaps:strip"]);
run(["bun", "run", "verify-build-identity"]);
run(["bun", "x", "wrangler", "deploy", "--config", "dist/server/wrangler.json"]);
