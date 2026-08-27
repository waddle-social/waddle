import { describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const repoRoot = resolve(import.meta.dir, "..", "..");
const checker = resolve(repoRoot, "scripts", "check-native-remote-telemetry.sh");

function runChecker(args: string[], root = repoRoot) {
  return spawnSync("bash", [checker, ...args], {
    cwd: repoRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      WADDLE_NATIVE_TELEMETRY_ROOT: root,
    },
  });
}

function writeFixtureFile(root: string, relativePath: string, contents: string) {
  const path = resolve(root, relativePath);
  mkdirSync(resolve(path, ".."), { recursive: true });
  writeFileSync(path, contents);
}

function writeSharedClientPathDependencyFixture(root: string) {
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-telemetry-helper/Cargo.toml",
    `
[package]
name = "waddle-xmpp-telemetry-helper"

[dependencies]
opentelemetry-otlp = "0.1"
`,
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-telemetry-helper/src/lib.rs",
    'pub fn helper() { let _collector = "https://telemetry.example/v1/traces"; }\n',
  );
}

function writeSharedClientFixture(root: string, overrides?: {
  clientCargoToml?: string;
  clientSrc?: string;
  coreCargoToml?: string;
  coreSrc?: string;
  ffiCargoToml?: string;
  ffiSrc?: string;
}) {
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client/Cargo.toml",
    overrides?.clientCargoToml ?? `
[package]
name = "waddle-xmpp-client"

[dependencies]
tracing = "0.1"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
`,
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client/src/lib.rs",
    overrides?.clientSrc ?? "pub fn client_fixture() {}\n",
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-core/Cargo.toml",
    overrides?.coreCargoToml ?? `
[package]
name = "waddle-xmpp-core"

[dependencies]
tracing = "0.1"
`,
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-core/src/lib.rs",
    overrides?.coreSrc ?? "pub fn core_fixture() {}\n",
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client-ffi/Cargo.toml",
    overrides?.ffiCargoToml ?? `
[package]
name = "waddle-xmpp-client-ffi"

[dependencies]
waddle-xmpp-client = { path = "../waddle-xmpp-client" }
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
`,
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client-ffi/src/lib.rs",
    overrides?.ffiSrc ?? "pub fn ffi_fixture() {}\n",
  );
}

function writeAndroidFixture(root: string, overrides?: {
  appBuildGradle?: string;
  buildGradle?: string;
  coreClientBuildGradle?: string;
  gradleProperties?: string;
  libsVersionsToml?: string;
  settingsGradle?: string;
}) {
  writeFixtureFile(root, "apps/android/build.gradle.kts", overrides?.buildGradle ?? "plugins {}\n");
  writeFixtureFile(
    root,
    "apps/android/app/build.gradle.kts",
    overrides?.appBuildGradle ?? "plugins {}\n",
  );
  writeFixtureFile(
    root,
    "apps/android/core/client/build.gradle.kts",
    overrides?.coreClientBuildGradle ?? "plugins {}\n",
  );
  writeFixtureFile(
    root,
    "apps/android/settings.gradle.kts",
    overrides?.settingsGradle ?? 'rootProject.name = "fixture"\ninclude(":app")\ninclude(":core:client")\n',
  );
  writeFixtureFile(
    root,
    "apps/android/app/src/main/AndroidManifest.xml",
    "<manifest package=\"social.waddle.fixture\" />\n",
  );
  writeFixtureFile(
    root,
    "apps/android/gradle/libs.versions.toml",
    overrides?.libsVersionsToml ?? "[plugins]\nandroid = { id = \"com.android.application\", version = \"1.0.0\" }\n",
  );
  if (overrides?.gradleProperties !== undefined) {
    writeFixtureFile(root, "apps/android/gradle.properties", overrides.gradleProperties);
  }
}

function writeWasmFixture(root: string, overrides?: {
  cargoToml?: string;
  driverSrc?: string;
  eventsSrc?: string;
  generatedJs?: string;
}) {
  writeSharedClientFixture(root);
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client-wasm/Cargo.toml",
    overrides?.cargoToml ?? `
[package]
name = "waddle-xmpp-client-wasm"

[dependencies]
tracing = "0.1"
`,
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client-wasm/src/events.rs",
    overrides?.eventsSrc ?? "pub fn local_callback() {}\n",
  );
  writeFixtureFile(
    root,
    "server/crates/waddle-xmpp-client-wasm/src/driver.rs",
    overrides?.driverSrc ?? "pub fn driver() {}\n",
  );
  if (overrides?.generatedJs !== undefined) {
    writeFixtureFile(
      root,
      "server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js",
      overrides.generatedJs,
    );
  }
}

function writeAppleFixture(root: string, overrides?: {
  appModelSwift?: string;
  projectPbxproj?: string;
  projectYml?: string;
  rustClientSwift?: string;
  supportSwift?: string;
}) {
  writeSharedClientFixture(root);
  writeFixtureFile(root, "apps/apple/project.yml", overrides?.projectYml ?? "name: Waddle\n");
  writeFixtureFile(
    root,
    "apps/apple/Waddle.xcodeproj/project.pbxproj",
    overrides?.projectPbxproj ?? "// project\n",
  );
  writeFixtureFile(
    root,
    "apps/apple/Waddle/App/AppModel.swift",
    overrides?.appModelSwift ?? "import OSLog\n",
  );
  writeFixtureFile(
    root,
    "apps/apple/Waddle/RustClient/RustXmppClient.swift",
    overrides?.rustClientSwift ?? "import OSLog\n",
  );
  if (overrides?.supportSwift !== undefined) {
    writeFixtureFile(root, "apps/apple/Waddle/Support/TelemetryBootstrap.swift", overrides.supportSwift);
  }
}

// The checker shells out to grep over the shared client crates; BSD grep on
// macOS is an order of magnitude slower than GNU grep, so give each
// invocation room beyond Bun's 5 s default.
const CHECKER_TIMEOUT_MS = 60_000;

describe("native remote telemetry contract", () => {
  test("WASM/browser surfaces remain free of remote telemetry exporters", () => {
    const result = runChecker(["--wasm"]);
    expect(result.status).toBe(0);
    expect(result.stdout.trim()).toContain("native remote telemetry contract OK: wasm");
  }, CHECKER_TIMEOUT_MS);

  test("checker validates every native surface in one pass", () => {
    const result = runChecker(["--all"]);
    expect(result.status).toBe(0);
    expect(result.stdout.trim()).toContain("native remote telemetry contract OK: all");
  }, CHECKER_TIMEOUT_MS);

  test("checker rejects unknown modes", () => {
    const result = runChecker(["--bogus"]);
    expect(result.status).toBe(2);
    expect(result.stderr).toContain("--apple|--android|--wasm|--all");
  }, CHECKER_TIMEOUT_MS);

  test("synthetic wasm fixture fails when a remote telemetry subscriber is added", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeWasmFixture(root, {
        cargoToml: `
[package]
name = "waddle-xmpp-client-wasm"

[dependencies]
tracing = "0.1"
tracing-subscriber = "0.3"
`,
      });

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic android fixture fails when crashlytics is added", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeSharedClientFixture(root);
      writeAndroidFixture(root, {
        appBuildGradle: 'plugins { id("com.google.firebase.crashlytics") }\n',
      });

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic apple bootstrap outside the known app files is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAppleFixture(root, {
        supportSwift: 'let collectorURL = "https://telemetry.example/v1/traces"\n',
      });

      const result = runChecker(["--apple"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("apple exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic generic collector configuration is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeSharedClientFixture(root);
      writeAndroidFixture(root, {
        gradleProperties: "COLLECTOR_URL=https://telemetry.example/collect\n",
      });

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic Android version-catalog telemetry alias is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeSharedClientFixture(root);
      writeAndroidFixture(root, {
        appBuildGradle: "plugins { alias(libs.plugins.telemetrySdk) }\n",
        libsVersionsToml: `
[plugins]
android-application = { id = "com.android.application", version = "9.3.0" }
telemetrySdk = { id = "io.sentry.android.gradle", version = "5.0.0" }
`,
      });

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic extra Android module with a collector endpoint is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeSharedClientFixture(root);
      writeAndroidFixture(root, {
        settingsGradle: `
rootProject.name = "fixture"
include(":app")
include(":core:client")
include(":feature:x")
`,
      });
      writeFixtureFile(root, "apps/android/feature/x/build.gradle.kts", "plugins {}\n");
      writeFixtureFile(
        root,
        "apps/android/feature/x/src/main/kotlin/X.kt",
        'const val COLLECTOR = "https://telemetry.example/v1/traces"\n',
      );

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client telemetry dependency is rejected in android mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
tracing = "0.1"
tracing-subscriber = "0.3"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
`,
      });
      writeAndroidFixture(root);

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared-client path dependency closure is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
tracing = "0.1"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
waddle-xmpp-telemetry-helper = { path = "../waddle-xmpp-telemetry-helper" }
`,
      });
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic workspace root crates.io patch is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
waddle-xmpp-telemetry-helper = "0.1"
`,
      });
      writeFixtureFile(
        root,
        "server/Cargo.toml",
        `
[workspace]
members = ["crates/*"]

[patch.crates-io]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic workspace root git-source patch is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeFixtureFile(
        root,
        "server/Cargo.toml",
        `
[workspace]
members = ["crates/*"]

[patch."https://github.com/example/telemetry"]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic workspace root replacement is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeFixtureFile(
        root,
        "server/Cargo.toml",
        `
[workspace]
members = ["crates/*"]

[replace]
"foo:0.1.0" = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic Cargo config paths override is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeFixtureFile(
        root,
        "server/.cargo/config.toml",
        'paths = ["crates/waddle-xmpp-telemetry-helper"]\n',
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic Cargo config patch is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeFixtureFile(
        root,
        "server/.cargo/config.toml",
        `
[patch.crates-io]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic workspace-inherited local dependency is rejected in every native mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
tracing = "0.1"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
waddle-xmpp-telemetry-helper = { workspace = true }
`,
      });
      writeFixtureFile(
        root,
        "server/Cargo.toml",
        `
[workspace]
members = ["crates/*"]

[workspace.dependencies.waddle-xmpp-telemetry-helper]
path = 'crates/waddle-xmpp-telemetry-helper'
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic quoted-key workspace-inherited dependency is rejected in every native mode", () => {
    const shapes = [
      {
        crate: `
[package]
name = "waddle-xmpp-client"

[dependencies]
tracing = "0.1"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }

[dependencies."waddle-xmpp-telemetry-helper"]
workspace = true
`,
        workspace: `
[workspace]
members = ["crates/*"]

[workspace.dependencies.waddle-xmpp-telemetry-helper]
path = "crates/waddle-xmpp-telemetry-helper"
`,
      },
      {
        crate: `
[package]
name = "waddle-xmpp-client"

[dependencies]
tracing = "0.1"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
"waddle-xmpp-telemetry-helper" = { workspace = true }
`,
        workspace: `
[workspace]
members = ["crates/*"]

[workspace.dependencies]
"waddle-xmpp-telemetry-helper" = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      },
    ];
    for (const shape of shapes) {
      const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
      try {
        writeAndroidFixture(root);
        writeAppleFixture(root);
        writeWasmFixture(root, { generatedJs: "export {};\n" });
        writeSharedClientFixture(root, { clientCargoToml: shape.crate });
        writeFixtureFile(root, "server/Cargo.toml", shape.workspace);
        writeSharedClientPathDependencyFixture(root);

        for (const mode of ["--apple", "--android", "--wasm"] as const) {
          const result = runChecker([mode], root);
          expect(result.status).toBe(1);
          expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
        }
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic whitespace-padded TOML headers still reach closure discovery", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies ]
tracing = "0.1"
waddle-xmpp-core = { path = "../waddle-xmpp-core" }

[ target . 'cfg(unix)' . dependencies ]
waddle-xmpp-telemetry-helper = { workspace = true }
`,
      });
      writeFixtureFile(
        root,
        "server/Cargo.toml",
        `
[ workspace ]
members = ["crates/*"]

[ workspace . dependencies ]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic quoted structural TOML headers still reach closure discovery", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAndroidFixture(root);
      writeAppleFixture(root);
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

["dependencies"]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
waddle-xmpp-telemetry-helper = { workspace = true }
`,
      });
      writeFixtureFile(
        root,
        "server/Cargo.toml",
        `
["workspace"]
members = ["crates/*"]

["workspace"."dependencies"]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      for (const mode of ["--apple", "--android", "--wasm"] as const) {
        const result = runChecker([mode], root);
        expect(result.status).toBe(1);
        expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
      }
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic escaped TOML path still reaches closure discovery", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
waddle-xmpp-telemetry-helper = { path = "\\u002e\\u002e/waddle-xmpp-telemetry-helper" }
`,
      });
      writeSharedClientPathDependencyFixture(root);

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic dev-only local dependency is excluded from the product closure", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }

[dev-dependencies]
local-helper = { path = "../local-helper" }
`,
      });
      writeFixtureFile(
        root,
        "server/crates/local-helper/Cargo.toml",
        '[package]\nname = "local-helper"\n\n[dependencies]\nopentelemetry-otlp = "0.1"\n',
      );
      writeFixtureFile(
        root,
        "server/crates/local-helper/src/lib.rs",
        'pub fn helper() { let _collector = "https://telemetry.example/v1/traces"; }\n',
      );

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(0);
      expect(result.stdout.trim()).toContain("native remote telemetry contract OK: wasm");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("malformed Cargo manifest in the local closure fails closed", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
local-helper = { path = "../local-helper" }
`,
      });
      writeFixtureFile(root, "server/crates/local-helper/Cargo.toml", "[package\nname = broken\n");
      writeFixtureFile(root, "server/crates/local-helper/src/lib.rs", "pub fn helper() {}\n");

      const result = runChecker(["--wasm"], root);
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain("failed to parse Cargo manifest");
      expect(result.stderr).toContain("local-helper/Cargo.toml");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client exporter init is rejected in apple mode", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAppleFixture(root);
      writeFixtureFile(
        root,
        "server/crates/waddle-xmpp-core/src/lib.rs",
        "pub fn install_exporter() { tracing_subscriber::fmt().init(); }\n",
      );

      const result = runChecker(["--apple"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("apple exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client lib target outside src is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeAppleFixture(root);
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[lib]
path = "ffi/lib.rs"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
`,
      });
      writeFixtureFile(
        root,
        "server/crates/waddle-xmpp-client/ffi/lib.rs",
        "pub fn install_exporter() { tracing_subscriber::fmt().init(); }\n",
      );

      const result = runChecker(["--apple"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("apple exporter");
      expect(result.stderr).toContain("ffi/lib.rs");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client build script collector is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeSharedClientFixture(root, {
        clientCargoToml: `
[package]
name = "waddle-xmpp-client"
build = "build.rs"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
`,
      });
      writeFixtureFile(
        root,
        "server/crates/waddle-xmpp-client/build.rs",
        'fn main() { let _collector = "https://telemetry.example/v1/traces"; }\n',
      );

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm exporter");
      expect(result.stderr).toContain("build.rs");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic generated wasm glue with a collector beacon is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      writeWasmFixture(root, {
        cargoToml: "[package]\nname = \"fixture\"\n",
        eventsSrc: "pub fn event() {}\n",
        generatedJs: 'fetch("https://telemetry.example/collect", { method: "POST" });\n',
      });

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
