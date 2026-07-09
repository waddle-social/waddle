import { afterEach, describe, expect, test } from "bun:test";
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  resolveBuildIdentity,
} from "../scripts/build-identity.mjs";
import { generateBuildIdentityArtifact } from "../scripts/generate-build-identity.mjs";
import { generateServiceWorker } from "../scripts/generate-service-worker.mjs";
import { verifyBuildArtifacts } from "../scripts/verify-build-identity.mjs";
import {
  disabledBuildIdentityMarker,
  faroBuildIdentityMarker,
  parseFaroBuildIdentityScope,
} from "../src/build-identity-contract";

const FULL_SHA = "0123456789abcdef0123456789abcdef01234567";
const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tempDirectories: string[] = [];
const faroEnv = {
  PUBLIC_FARO_URL: "https://faro.example/collect/source",
  PUBLIC_FARO_APP_NAME: "waddle-chat",
  PUBLIC_FARO_DEPLOYMENT_ENVIRONMENT: "production",
  PUBLIC_FARO_CLUSTER: "waddle-cloud",
  PUBLIC_FARO_NAMESPACE: "waddle",
  PUBLIC_FARO_SOURCE_ID: "waddle-chat",
};

afterEach(() => {
  for (const directory of tempDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

function temporaryProject(): string {
  const directory = mkdtempSync(resolve(tmpdir(), "waddle-chat-build-identity-"));
  tempDirectories.push(directory);
  return directory;
}

function gitExecutor(status = "") {
  return (command: string) => {
    switch (command) {
      case "git rev-parse --is-inside-work-tree":
        return Buffer.from("true\n");
      case "git rev-parse HEAD":
        return Buffer.from(`${FULL_SHA}\n`);
      case "git status --porcelain --untracked-files=normal":
        return Buffer.from(status);
      default:
        throw new Error(`unexpected command: ${command}`);
    }
  };
}

function noGit() {
  throw new Error("not a git checkout");
}

function fixtureIdentity() {
  return resolveBuildIdentity({ env: faroEnv, execSync: gitExecutor() });
}

function writeServiceWorkerTemplate(projectRoot: string): void {
  const directory = resolve(projectRoot, "src", "service-worker");
  mkdirSync(directory, { recursive: true });
  writeFileSync(
    resolve(directory, "sw-template.js"),
    'const CACHE_NAME = "waddle-__WADDLE_BUILD_SHA__";\n',
  );
}

function materializeClientFixture(
  projectRoot: string,
  clientMarker: string,
  workerMarker = clientMarker,
): void {
  const clientDir = resolve(projectRoot, "dist", "client");
  mkdirSync(resolve(clientDir, "_astro"), { recursive: true });
  cpSync(resolve(projectRoot, "public", "build-identity.json"), resolve(clientDir, "build-identity.json"));
  cpSync(resolve(projectRoot, "public", "sw.js"), resolve(clientDir, "sw.js"));
  writeFileSync(
    resolve(clientDir, "_astro", "app.js"),
    `export const marker=${JSON.stringify(clientMarker)};\n`,
  );

  const serverChunks = resolve(projectRoot, "dist", "server", "chunks");
  mkdirSync(serverChunks, { recursive: true });
  const manifest = {
    assets: ["/_astro/app.js"],
    entryModules: {
      "/fixture/src/layouts/AppLayout.astro?astro&type=script&index=0&lang.ts": "_astro/app.js",
    },
  };
  writeFileSync(
    resolve(serverChunks, "worker-entry_fixture.mjs"),
    [
      `const workerMarker=${JSON.stringify(workerMarker)};`,
      `const _manifest = deserializeManifest(${JSON.stringify(manifest)});`,
      "const manifestRoutes = _manifest.routes;",
    ].join("\n"),
  );
  const serverDir = resolve(projectRoot, "dist", "server");
  writeFileSync(
    resolve(serverDir, "wrangler.json"),
    `${JSON.stringify({
      main: "entry.mjs",
      no_bundle: true,
      assets: { binding: "ASSETS", directory: "../client" },
    })}\n`,
  );
  writeFileSync(
    resolve(serverDir, "entry.mjs"),
    [
      "globalThis.process ??= {};",
      "globalThis.process.env ??= {};",
      'import "cloudflare:workers";',
      'import { w } from "./chunks/worker-entry_fixture.mjs";',
      "export { w as default };",
    ].join("\n"),
  );
}

describe("web build identity", () => {
  test("shares one Faro scope and marker contract with browser telemetry", () => {
    const identity = fixtureIdentity();
    const scope = parseFaroBuildIdentityScope(
      identity.faro.scope,
      FULL_SHA,
      "test Faro scope",
    );

    expect(identity.marker).toBe(faroBuildIdentityMarker(FULL_SHA, scope));
  });

  test("marker contract rejects noncanonical commits and names disabled unknown explicitly", () => {
    const scope = fixtureIdentity().faro.scope;
    for (const commit of ["short", "A".repeat(40), "g".repeat(40)]) {
      expect(() => faroBuildIdentityMarker(commit, scope)).toThrow(
        "must be a full lowercase Git commit SHA",
      );
      expect(() => disabledBuildIdentityMarker(commit)).toThrow(
        "must be a full lowercase Git commit SHA",
      );
    }
    expect(disabledBuildIdentityMarker("unknown")).toBe(
      "waddle-build-identity-v1:unknown:faro-disabled",
    );
  });

  test("binds the full commit and exact static Faro deployment scope", () => {
    const identity = fixtureIdentity();

    expect(identity).toEqual({
      schemaVersion: 1,
      marker: [
        "waddle-build-identity-v1",
        FULL_SHA,
        FULL_SHA,
        "waddle-chat",
        "production",
        "waddle-cloud",
        "waddle",
      ].join(":"),
      commitSha: FULL_SHA,
      source: { kind: "git", commitSha: FULL_SHA },
      faro: {
        enabled: true,
        application: "waddle-chat",
        release: FULL_SHA,
        scope: {
          deploymentEnvironment: "production",
          cluster: "waddle-cloud",
          namespace: "waddle",
          sourceId: "waddle-chat",
          release: FULL_SHA,
        },
      },
    });
  });

  test("fails closed for missing, malformed, or inconsistent Faro scope", () => {
    for (const [name, value] of [
      ["PUBLIC_FARO_CLUSTER", ""],
      ["PUBLIC_FARO_NAMESPACE", "unsafe/value"],
      ["PUBLIC_FARO_SOURCE_ID", "another-source"],
    ]) {
      expect(() => resolveBuildIdentity({
        env: { ...faroEnv, [name]: value },
        execSync: gitExecutor(),
      })).toThrow();
    }

    for (const url of [
      "not-a-url",
      "http://faro.example/collect",
      "https://user:secret@faro.example/collect",
      "https://faro.example/collect?token=secret",
      "https://faro.example/collect#private",
    ]) {
      expect(() => resolveBuildIdentity({
        env: { ...faroEnv, PUBLIC_FARO_URL: url },
        execSync: gitExecutor(),
      })).toThrow("PUBLIC_FARO_URL");
    }
  });

  test("production deploy mode requires Faro and immutable Git provenance", () => {
    expect(() => resolveBuildIdentity({
      env: {
        WADDLE_REQUIRE_IMMUTABLE_BUILD: "true",
        WADDLE_REQUIRE_FARO_BUILD: "true",
      },
      execSync: gitExecutor(),
    })).toThrow("production deploys require PUBLIC_FARO_URL");

    expect(() => resolveBuildIdentity({
      env: {
        ...faroEnv,
        WADDLE_REQUIRE_IMMUTABLE_BUILD: "true",
        WADDLE_REQUIRE_FARO_BUILD: "true",
      },
      execSync: gitExecutor(" M src/app.ts\n"),
    })).toThrow("production/Faro builds require a clean git worktree");

    expect(() => resolveBuildIdentity({
      env: {
        ...faroEnv,
        WADDLE_REQUIRE_IMMUTABLE_BUILD: "true",
        WADDLE_REQUIRE_FARO_BUILD: "true",
        WADDLE_GIT_SHA: FULL_SHA,
      },
      execSync: noGit,
    })).toThrow("cannot attest a source archive");
  });

  test("generates and verifies one identity across artifact, service worker, and client bundle", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(projectRoot, identity.marker);

    const verified = verifyBuildArtifacts({ projectRoot, expectedIdentity: identity });
    expect(verified.identity).toEqual(identity);
    expect(readFileSync(verified.serviceWorkerPath, "utf8")).toContain(
      `const CACHE_NAME = "waddle-${FULL_SHA}";`,
    );
    expect(verified.markerFile.endsWith("/_astro/app.js")).toBe(true);
  });

  test("verifier rejects independently stale artifacts", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(
      projectRoot,
      "waddle-build-identity-v1:stale",
      identity.marker,
    );

    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "contains a stale or inconsistent build identity marker",
    );

    writeFileSync(
      resolve(projectRoot, "dist", "client", "_astro", "app.js"),
      `export const marker=${JSON.stringify(identity.marker)};\n`,
    );
    writeFileSync(
      resolve(projectRoot, "dist", "client", "sw.js"),
      'const CACHE_NAME = "waddle-stale";\n',
    );
    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      `service worker must contain exactly one top-level immutable cache identity waddle-${FULL_SHA}`,
    );
  });

  test("verifier ignores comment and template-string service-worker cache decoys", () => {
    const decoyKinds = ["comment", "template"] as const;
    for (const decoyKind of decoyKinds) {
      const projectRoot = temporaryProject();
      const identity = fixtureIdentity();
      writeServiceWorkerTemplate(projectRoot);
      generateBuildIdentityArtifact({ projectRoot, identity });
      generateServiceWorker({ projectRoot, identity });
      materializeClientFixture(projectRoot, identity.marker);
      const decoySource = `const CACHE_NAME = "waddle-${FULL_SHA}";`;
      const decoy = decoyKind === "comment"
        ? `/* ${decoySource} */\n`
        : `const cacheIdentityDecoy = \`${decoySource}\`;\n`;
      writeFileSync(resolve(projectRoot, "dist", "client", "sw.js"), decoy);

      expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
        `service worker must contain exactly one top-level immutable cache identity waddle-${FULL_SHA}`,
      );
    }
  });

  test("verifier rejects ambiguous or placeholder-bearing service-worker cache identity", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(projectRoot, identity.marker);
    const serviceWorkerPath = resolve(projectRoot, "dist", "client", "sw.js");
    writeFileSync(
      serviceWorkerPath,
      [
        `var CACHE_NAME = "waddle-${FULL_SHA}";`,
        `var CACHE_NAME = "waddle-${FULL_SHA}";`,
      ].join("\n"),
    );
    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      `service worker must contain exactly one top-level immutable cache identity waddle-${FULL_SHA}`,
    );

    writeFileSync(
      serviceWorkerPath,
      `const CACHE_NAME = "waddle-${FULL_SHA}";\n/* __WADDLE_BUILD_SHA__ */\n`,
    );
    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      `service worker must contain exactly one top-level immutable cache identity waddle-${FULL_SHA}`,
    );
  });

  test("verifier rejects an unreferenced marker decoy and symbolic-link artifacts", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(
      projectRoot,
      "waddle-build-identity-v1:stale",
      identity.marker,
    );
    writeFileSync(
      resolve(projectRoot, "dist", "client", "_astro", "decoy.js"),
      `export const marker=${JSON.stringify(identity.marker)};\n`,
    );

    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "client JavaScript assets do not match the server manifest",
    );

    rmSync(resolve(projectRoot, "dist", "client", "build-identity.json"));
    symlinkSync(
      resolve(projectRoot, "public", "build-identity.json"),
      resolve(projectRoot, "dist", "client", "build-identity.json"),
    );
    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "expected a regular file",
    );
  });

  test("verifier rejects symbolic-link ancestors for the build trees", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(projectRoot, identity.marker);

    const dist = resolve(projectRoot, "dist");
    const redirectedDist = resolve(projectRoot, "redirected-dist");
    cpSync(dist, redirectedDist, { recursive: true });
    rmSync(dist, { recursive: true });
    symlinkSync(redirectedDist, dist);

    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "build path contains a symbolic-link ancestor",
    );
  });

  test("verifier rejects duplicate-key build identity JSON even when JSON.parse selects expected values", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(projectRoot, identity.marker);

    const artifactPath = resolve(projectRoot, "dist", "client", "build-identity.json");
    const canonical = readFileSync(artifactPath, "utf8");
    writeFileSync(
      artifactPath,
      canonical.replace(
        `  "commitSha": "${FULL_SHA}",`,
        `  "commitSha": "${"f".repeat(40)}",\n  "commitSha": "${FULL_SHA}",`,
      ),
    );

    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "is not the canonical expected build identity",
    );
  });

  test("verifier ignores comment and template-string Astro manifest decoys", () => {
    const decoyKinds = ["comment", "template"] as const;
    for (const decoyKind of decoyKinds) {
      const projectRoot = temporaryProject();
      const identity = fixtureIdentity();
      writeServiceWorkerTemplate(projectRoot);
      generateBuildIdentityArtifact({ projectRoot, identity });
      generateServiceWorker({ projectRoot, identity });
      materializeClientFixture(projectRoot, identity.marker);

      const validManifest = {
        assets: ["/_astro/app.js"],
        entryModules: {
          "/fixture/src/layouts/AppLayout.astro?astro&type=script&index=0&lang.ts": "_astro/app.js",
        },
      };
      const staleManifest = {
        assets: ["/_astro/stale.js"],
        entryModules: {
          "/fixture/src/layouts/AppLayout.astro?astro&type=script&index=0&lang.ts": "_astro/stale.js",
        },
      };
      const decoySource = [
        `const _manifest = deserializeManifest(${JSON.stringify(validManifest)});`,
        "const manifestRoutes",
      ].join("\n");
      const decoy = decoyKind === "comment"
        ? `/*\n${decoySource}\n*/`
        : `const manifestDecoy = \`${decoySource}\`;`;
      const workerPath = resolve(projectRoot, "dist", "server", "chunks", "worker-entry_fixture.mjs");
      writeFileSync(
        workerPath,
        [
          `const workerMarker=${JSON.stringify(identity.marker)};`,
          decoy,
          `const _manifest = deserializeManifest(${JSON.stringify(staleManifest)});`,
          "const manifestRoutes = _manifest.routes;",
        ].join("\n"),
      );

      expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
        "client JavaScript assets do not match the server manifest",
      );
    }
  });

  test("verifier requires one unambiguous top-level Astro manifest binding", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(projectRoot, identity.marker);
    const manifest = {
      assets: ["/_astro/app.js"],
      entryModules: {
        "/fixture/src/layouts/AppLayout.astro?astro&type=script&index=0&lang.ts": "_astro/app.js",
      },
    };
    writeFileSync(
      resolve(projectRoot, "dist", "server", "chunks", "worker-entry_fixture.mjs"),
      [
        `const workerMarker=${JSON.stringify(identity.marker)};`,
        `var _manifest = deserializeManifest(${JSON.stringify(manifest)});`,
        `var _manifest = deserializeManifest(${JSON.stringify(manifest)});`,
        "const manifestRoutes = _manifest.routes;",
      ].join("\n"),
    );

    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "must contain exactly one top-level Astro manifest binding",
    );
  });

  test("verifier requires a real marker literal in the manifest-selected AppLayout entry", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });
    materializeClientFixture(projectRoot, identity.marker);
    writeFileSync(
      resolve(projectRoot, "dist", "client", "_astro", "app.js"),
      `/* ${identity.marker} */\nexport const marker = "not-an-identity";\n`,
    );

    expect(() => verifyBuildArtifacts({ projectRoot, expectedIdentity: identity })).toThrow(
      "AppLayout client entry must contain exactly one expected build identity marker literal",
    );
  });

  test("verifier rejects generated Wrangler config redirection or changed upload semantics", () => {
    for (const [mutate, expectedError] of [
      [
        (config: Record<string, unknown>) => { config.main = "redirected-entry.mjs"; },
        "must set main to entry.mjs",
      ],
      [
        (config: Record<string, unknown>) => { config.no_bundle = false; },
        "must enable no_bundle",
      ],
      [
        (config: Record<string, unknown>) => {
          config.assets = { binding: "ASSETS", directory: "../redirected-client" };
        },
        "must bind ASSETS to ../client",
      ],
    ] as const) {
      const fixtureRoot = temporaryProject();
      const identity = fixtureIdentity();
      writeServiceWorkerTemplate(fixtureRoot);
      generateBuildIdentityArtifact({ projectRoot: fixtureRoot, identity });
      generateServiceWorker({ projectRoot: fixtureRoot, identity });
      materializeClientFixture(fixtureRoot, identity.marker);
      const configPath = resolve(fixtureRoot, "dist", "server", "wrangler.json");
      const config = JSON.parse(readFileSync(configPath, "utf8")) as Record<string, unknown>;
      mutate(config);
      writeFileSync(configPath, `${JSON.stringify(config)}\n`);

      expect(() => verifyBuildArtifacts({ projectRoot: fixtureRoot, expectedIdentity: identity })).toThrow(
        expectedError,
      );
    }
  });

  test("verifier rejects Wrangler entry import or default-export redirection", () => {
    for (const [entrySource, expectedError] of [
      [
        [
          "globalThis.process ??= {};",
          "globalThis.process.env ??= {};",
          'import "cloudflare:workers";',
          'import { w } from "./chunks/worker-entry_redirected.mjs";',
          "export { w as default };",
        ].join("\n"),
        "must import exactly the verified worker entry",
      ],
      [
        [
          "globalThis.process ??= {};",
          "globalThis.process.env ??= {};",
          'import "cloudflare:workers";',
          'import { w } from "./chunks/worker-entry_fixture.mjs";',
          "export { w as redirected };",
        ].join("\n"),
        "must export only the verified worker as default",
      ],
    ] as const) {
      const fixtureRoot = temporaryProject();
      const identity = fixtureIdentity();
      writeServiceWorkerTemplate(fixtureRoot);
      generateBuildIdentityArtifact({ projectRoot: fixtureRoot, identity });
      generateServiceWorker({ projectRoot: fixtureRoot, identity });
      materializeClientFixture(fixtureRoot, identity.marker);
      writeFileSync(resolve(fixtureRoot, "dist", "server", "entry.mjs"), entrySource);

      expect(() => verifyBuildArtifacts({ projectRoot: fixtureRoot, expectedIdentity: identity })).toThrow(
        expectedError,
      );
    }
  });

  test("verifier rejects side-effect import redirection or injected entrypoint code", () => {
    for (const entrySource of [
      [
        "globalThis.process ??= {};",
        "globalThis.process.env ??= {};",
        'import "./redirected-side-effect.mjs";',
        'import { w } from "./chunks/worker-entry_fixture.mjs";',
        "export { w as default };",
      ].join("\n"),
      [
        "globalThis.process ??= {};",
        "globalThis.process.env ??= {};",
        'import "cloudflare:workers";',
        'import { w } from "./chunks/worker-entry_fixture.mjs";',
        "redirectedTopLevelCall();",
        "export { w as default };",
      ].join("\n"),
    ]) {
      const fixtureRoot = temporaryProject();
      const identity = fixtureIdentity();
      writeServiceWorkerTemplate(fixtureRoot);
      generateBuildIdentityArtifact({ projectRoot: fixtureRoot, identity });
      generateServiceWorker({ projectRoot: fixtureRoot, identity });
      materializeClientFixture(fixtureRoot, identity.marker);
      writeFileSync(resolve(fixtureRoot, "dist", "server", "entry.mjs"), entrySource);

      expect(() => verifyBuildArtifacts({ projectRoot: fixtureRoot, expectedIdentity: identity })).toThrow(
        "generated Wrangler entry must",
      );
    }
  });

  test("ignored generated sources leave a production checkout clean", () => {
    const projectRoot = temporaryProject();
    const identity = fixtureIdentity();
    writeServiceWorkerTemplate(projectRoot);
    writeFileSync(
      resolve(projectRoot, ".gitignore"),
      "public/build-identity.json\npublic/sw.js\n",
    );
    runGit(projectRoot, ["init", "--quiet"]);
    runGit(projectRoot, ["add", ".gitignore", "src/service-worker/sw-template.js"]);
    runGit(projectRoot, [
      "-c", "user.name=Waddle Tests",
      "-c", "user.email=waddle-tests@example.invalid",
      "-c", "commit.gpgsign=false",
      "commit", "--quiet", "-m", "fixture",
    ]);

    generateBuildIdentityArtifact({ projectRoot, identity });
    generateServiceWorker({ projectRoot, identity });

    const status = runGit(projectRoot, ["status", "--porcelain", "--untracked-files=all"]);
    expect(status).toBe("");
  });

  test("standard and cuenv deploy paths cannot bypass artifact verification", () => {
    const packageJson = JSON.parse(readFileSync(resolve(projectRoot, "package.json"), "utf8")) as {
      scripts: Record<string, string>;
    };
    const build = packageJson.scripts.build;
    expect(build.indexOf("generate-build-identity")).toBeLessThan(build.indexOf("astro build"));
    expect(build.indexOf("generate-service-worker")).toBeLessThan(build.indexOf("astro build"));
    expect(build.indexOf("verify-build-identity")).toBeGreaterThan(build.indexOf("astro build"));
    expect(packageJson.scripts.deploy).toBe("bun run scripts/deploy.mjs");

    const deployScript = readFileSync(resolve(projectRoot, "scripts", "deploy.mjs"), "utf8");
    expect(deployScript).toContain('REBUILD_WASM: "1"');
    expect(deployScript).toContain('WADDLE_REQUIRE_IMMUTABLE_BUILD: "true"');
    expect(deployScript).toContain('WADDLE_REQUIRE_FARO_BUILD: "true"');
    const clean = deployScript.indexOf('rmSync(resolve(projectRoot, "dist")');
    const generateTypes = deployScript.indexOf('run(["bun", "run", "generate-types"])');
    const rebuild = deployScript.indexOf('run(["bun", "run", "build"])');
    const strip = deployScript.indexOf('run(["bun", "run", "sourcemaps:strip"])');
    const verify = deployScript.indexOf('run(["bun", "run", "verify-build-identity"])');
    const deploy = deployScript.indexOf(
      'run(["bun", "x", "wrangler", "deploy", "--config", "dist/server/wrangler.json"])',
    );
    expect(clean).toBeGreaterThan(-1);
    expect(generateTypes).toBeGreaterThan(clean);
    expect(rebuild).toBeGreaterThan(generateTypes);
    expect(strip).toBeGreaterThan(rebuild);
    expect(verify).toBeGreaterThan(strip);
    expect(deploy).toBeGreaterThan(verify);

    const cue = readFileSync(resolve(projectRoot, "env.cue"), "utf8");
    expect(cue).toContain("verifyBuildIdentity: schema.#Task");
    expect(cue).toMatch(/stripSourcemaps:[\s\S]*?dependsOn: \[verifyBuildIdentity\]/);
    const deployTask = cue.slice(cue.indexOf("\n\t\tdeploy:"), cue.indexOf("\n\t}\n\n\tservices:"));
    expect(deployTask).toContain('command: "bun"');
    expect(deployTask).toContain('args: ["run", "deploy"]');
    expect(deployTask).toContain('cache: mode: "never"');
    expect(deployTask).toContain('"scripts/**"');
    expect(deployTask).toContain('"src/**"');
    expect(deployTask).toContain('"wrangler.jsonc"');
    expect(deployTask).not.toContain("dependsOn:");
    expect(deployTask).not.toContain('"dist/**"');
    expect(cue.match(/cache: mode: "never"/g)).toHaveLength(3);

    const gitignore = readFileSync(resolve(projectRoot, ".gitignore"), "utf8");
    expect(gitignore).toContain("public/build-identity.json");
    expect(gitignore).toContain("public/sw.js");
  });
});

function runGit(projectRoot: string, argumentsList: string[]): string {
  const result = Bun.spawnSync(["git", ...argumentsList], {
    cwd: projectRoot,
    stdout: "pipe",
    stderr: "pipe",
  });
  if (result.exitCode !== 0) {
    throw new Error(new TextDecoder().decode(result.stderr));
  }
  return new TextDecoder().decode(result.stdout).trim();
}
