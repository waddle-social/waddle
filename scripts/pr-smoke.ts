// This job runs without installing workspace dependencies or building artifacts.
import { lstatSync, readFileSync } from "node:fs";
import { basename, extname } from "node:path";

function run(args: string[], quiet = false): string {
  const result = Bun.spawnSync(args, { stdout: quiet ? "pipe" : "inherit", stderr: "inherit" });
  if (result.exitCode !== 0) throw new Error(`${args[0]} failed (${result.exitCode})`);
  return result.stdout?.toString() ?? "";
}

const [base, head] = Bun.argv.slice(2);
if (![base, head].every(sha => /^[0-9a-f]{40}$/.test(sha ?? ""))) throw new Error("Expected full base and candidate commit IDs");
if (run(["git", "rev-parse", "HEAD"], true).trim() !== head) throw new Error("Checkout differs from the event candidate");
run(["git", "merge-base", "--is-ancestor", base, head]);
run(["git", "diff", "--check", base, head]);
const paths = run(["git", "diff", "--no-renames", "--name-only", "-z", base, head], true).split("\0").filter(Boolean);
const affected = (...patterns: string[]) => patterns.some(pattern => paths.some(path => new Bun.Glob(pattern).match(path)));
let checked = 0;
for (const path of paths) {
  if (!lstatSync(path, { throwIfNoEntry: false })?.isFile()) continue;
  const ext = extname(path);
  try {
    if (/\.[cm]?[jt]sx?$/.test(path) && !path.endsWith(".d.ts")) {
      const loader = ext.replace(/^[.][cm]?/, "") as "ts" | "tsx" | "js" | "jsx";
      new Bun.Transpiler({ loader }).transformSync(readFileSync(path, "utf8"));
    } else if (path.endsWith(".toml") || path === "cuenv.lock" || basename(path) === "Cargo.lock") {
      Bun.TOML.parse(readFileSync(path, "utf8"));
    } else if (path.endsWith(".json") || path.endsWith(".jsonc") || path === "bun.lock") {
      const jsonc = path.endsWith(".jsonc") || path === "bun.lock" || /^tsconfig(?:\..+)?\.json$/.test(basename(path));
      const source = readFileSync(path, "utf8");
      if (jsonc) Bun.JSONC.parse(source);
      else JSON.parse(source);
    } else if (path.startsWith(".github/workflows/") && /\.ya?ml$/.test(path)) {
      Bun.YAML.parse(readFileSync(path, "utf8"));
    } else if (ext === ".sh") {
      run(["bash", "-n", "--", path]);
    } else if (ext === ".py") {
      run(["python3", "-c", "import ast, sys, tokenize; p = sys.argv[1]; ast.parse(tokenize.open(p).read(), p)", path]);
    } else if (ext === ".rs" && !path.startsWith("server/vendor/")) {
      // Parse the edited module in isolation. The queue checks workspace formatting and compiles it.
      run(["rustfmt", "--edition", "2024", "--config", "skip_children=true", "--emit", "stdout", "--", path], true);
    } else continue;
    checked++;
  } catch (error) {
    throw new Error(`Source check failed for ${path}`, { cause: error });
  }
}
console.log(`Checked the diff for ${paths.length} changed paths and parsed ${checked} source files.`);

if (affected("apps/apple/**", "apps/android/**", "chat/**", "server/crates/waddle-xmpp-client*/**", "server/crates/waddle-xmpp-core/**", "scripts/*telemetry*")) {
  run(["bash", "scripts/check-native-remote-telemetry.sh", "--all"]);
}
if (affected("design/**", "scripts/design-tokens/**", "scripts/generate-design-tokens.mjs")) {
  run([process.execPath, "test", "scripts/design-tokens"]);
}
if (affected("chat/scripts/build-xmpp-wasm.mjs", "chat/env.cue", "chat/package.json", "chat/tests/wasm-ui-build-contract.test.ts")) {
  run([process.execPath, "test", "chat/tests/wasm-ui-build-contract.test.ts"]);
}
if (affected(".github/workflows/**", "scripts/integration-ci*", "scripts/pr-smoke.ts", "ci/rust-tests/**")) {
  run([process.execPath, "scripts/integration-ci.ts", "--check"]);
  run([process.execPath, "test", "scripts/integration-ci.test.ts"]);
}

if (affected("infrastructure/github/**")) {
  run(["python3", "-B", "-m", "unittest", "discover", "-s", "infrastructure/github", "-p", "test_*.py"]);
}
