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

function withFixtureRoot(run: (root: string) => void): void {
  const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
  try {
    run(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function writeAllPlatformFixtures(root: string): void {
  writeAndroidFixture(root);
  writeAppleFixture(root);
  writeWasmFixture(root, { generatedJs: "export {};\n" });
}

function expectRejectedInEveryNativeMode(root: string): void {
  for (const mode of ["--apple", "--android", "--wasm"] as const) {
    const result = runChecker([mode], root);
    expect(result.status).toBe(1);
    expect(result.stderr).toContain(`${mode.slice(2)} dependency`);
  }
}

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
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic android fixture fails when crashlytics is added", () => {
    withFixtureRoot((root) => {
      writeSharedClientFixture(root);
      writeAndroidFixture(root, {
        appBuildGradle: 'plugins { id("com.google.firebase.crashlytics") }\n',
      });

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android dependency");
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic apple bootstrap outside the known app files is rejected", () => {
    withFixtureRoot((root) => {
      writeAppleFixture(root, {
        supportSwift: 'let collectorURL = "https://telemetry.example/v1/traces"\n',
      });

      const result = runChecker(["--apple"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("apple exporter");
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic generic collector configuration is rejected", () => {
    withFixtureRoot((root) => {
      writeSharedClientFixture(root);
      writeAndroidFixture(root, {
        gradleProperties: "COLLECTOR_URL=https://telemetry.example/collect\n",
      });

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android exporter");
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic Android version-catalog telemetry alias is rejected", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic extra Android module with a collector endpoint is rejected", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client telemetry dependency is rejected in android mode", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared-client path dependency closure is rejected in every native mode", () => {
    withFixtureRoot((root) => {
      writeAllPlatformFixtures(root);
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

      expectRejectedInEveryNativeMode(root);
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic local git-file dependency, patch, and replacement are rejected in wasm mode", () => {
    const shapes = [
      {
        name: "dependency",
        clientDependency:
          'waddle-xmpp-telemetry-helper = { git = "file:../waddle-xmpp-telemetry-helper" }',
        rootManifest: undefined,
      },
      {
        name: "patch",
        clientDependency: 'waddle-xmpp-telemetry-helper = "0.1"',
        rootManifest: `
[workspace]
members = ["crates/*"]

[patch.crates-io]
waddle-xmpp-telemetry-helper = { git = "file:crates/waddle-xmpp-telemetry-helper" }
`,
      },
      {
        name: "replacement",
        clientDependency: "",
        rootManifest: `
[workspace]
members = ["crates/*"]

[replace]
"foo:0.1.0" = { git = "file://crates/waddle-xmpp-telemetry-helper" }
`,
      },
    ];

    for (const shape of shapes) {
      withFixtureRoot((root) => {
        writeWasmFixture(root, { generatedJs: "export {};\n" });
        writeSharedClientFixture(root, {
          clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
${shape.clientDependency}
`,
        });
        if (shape.rootManifest !== undefined) {
          writeFixtureFile(root, "server/Cargo.toml", shape.rootManifest);
        }
        writeSharedClientPathDependencyFixture(root);

        const result = runChecker(["--wasm"], root);
        expect(result.status, shape.name).toBe(1);
        expect(result.stderr, shape.name).toContain("wasm dependency");
      });
    }
  }, CHECKER_TIMEOUT_MS);

  test("missing and bare local git-file sources fail closed", () => {
    for (const target of ["missing-helper", "bare-helper"] as const) {
      withFixtureRoot((root) => {
        writeWasmFixture(root, { generatedJs: "export {};\n" });
        writeSharedClientFixture(root, {
          clientCargoToml: `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
helper = { git = "file:../${target}" }
`,
        });
        if (target === "bare-helper") {
          writeFixtureFile(root, "server/crates/bare-helper/HEAD", "ref: refs/heads/main\n");
          mkdirSync(resolve(root, "server/crates/bare-helper/objects"), { recursive: true });
        }

        const result = runChecker(["--wasm"], root);
        expect(result.status, target).not.toBe(0);
        expect(result.stderr, target).toContain("local git file source");
        expect(result.stderr, target).toContain(
          target === "bare-helper" ? "bare git repository" : "missing directory",
        );
      });
    }
  }, CHECKER_TIMEOUT_MS);

  const everyNativeModeCases: Array<{
    name: string;
    files: Array<readonly [relativePath: string, contents: string]>;
  }> = [
    {
      name: "synthetic workspace root crates.io patch is rejected in every native mode",
      files: [["server/crates/waddle-xmpp-client/Cargo.toml", `
[package]
name = "waddle-xmpp-client"

[dependencies]
waddle-xmpp-core = { path = "../waddle-xmpp-core" }
waddle-xmpp-telemetry-helper = "0.1"
`], ["server/Cargo.toml", `
[workspace]
members = ["crates/*"]

[patch.crates-io]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`]],
    },
    {
      name: "synthetic workspace root git-source patch is rejected in every native mode",
      files: [["server/Cargo.toml", `
[workspace]
members = ["crates/*"]

[patch."https://github.com/example/telemetry"]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`]],
    },
    {
      name: "synthetic workspace root replacement is rejected in every native mode",
      files: [["server/Cargo.toml", `
[workspace]
members = ["crates/*"]

[replace]
"foo:0.1.0" = { path = "crates/waddle-xmpp-telemetry-helper" }
`]],
    },
    {
      name: "synthetic Cargo config paths override is rejected in every native mode",
      files: [["server/.cargo/config.toml", 'paths = ["crates/waddle-xmpp-telemetry-helper"]\n']],
    },
  ];

  for (const fixtureCase of everyNativeModeCases) {
    test(fixtureCase.name, () => {
      withFixtureRoot((root) => {
        writeAllPlatformFixtures(root);
        for (const [relativePath, contents] of fixtureCase.files) {
          writeFixtureFile(root, relativePath, contents);
        }
        writeSharedClientPathDependencyFixture(root);

        expectRejectedInEveryNativeMode(root);
      });
    }, CHECKER_TIMEOUT_MS);
  }

  test("synthetic preferred extensionless Cargo config is rejected in wasm mode", () => {
    withFixtureRoot((root) => {
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeFixtureFile(root, "server/.cargo/config.toml", "[net]\noffline = true\n");
      writeFixtureFile(
        root,
        "server/.cargo/config",
        'paths = ["crates/waddle-xmpp-telemetry-helper"]\n',
      );
      writeSharedClientPathDependencyFixture(root);

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm dependency");
    });
  }, CHECKER_TIMEOUT_MS);

  test("extensionless Cargo config shadows config.toml at the same level", () => {
    withFixtureRoot((root) => {
      writeWasmFixture(root, { generatedJs: "export {};\n" });
      writeFixtureFile(root, "server/.cargo/config", "[net]\noffline = true\n");
      writeFixtureFile(
        root,
        "server/.cargo/config.toml",
        'paths = ["crates/waddle-xmpp-telemetry-helper"]\n',
      );
      writeSharedClientPathDependencyFixture(root);

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(0);
      expect(result.stdout).toContain("native remote telemetry contract OK: wasm");
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic Cargo config local source mechanisms are rejected in wasm mode", () => {
    const shapes: Array<{
      name: string;
      config: string;
      writeSource: (root: string) => void;
      expectMessage?: string;
    }> = [
      {
        name: "directory source",
        config: '[source.vendor]\ndirectory = "vendor"\n',
        writeSource(root: string) {
          writeFixtureFile(
            root,
            "server/vendor/helper/Cargo.toml",
            '[package]\nname = "helper"\n\n[dependencies]\nopentelemetry-otlp = "0.1"\n',
          );
        },
      },
      {
        name: "local registry source with archived crate",
        config: '[source.local]\nlocal-registry = "registry"\n',
        writeSource(root: string) {
          // A real local registry keeps sources inside compressed *.crate
          // archives the scanner cannot read; the closure must fail closed
          // even though nothing in plain text mentions telemetry.
          writeFixtureFile(root, "server/registry/index/he/lp/helper", "{}\n");
          const archive = Bun.gzipSync(new TextEncoder().encode("helper-0.1.0/Cargo.toml"));
          mkdirSync(resolve(root, "server/registry"), { recursive: true });
          writeFileSync(resolve(root, "server/registry/helper-0.1.0.crate"), archive);
        },
        expectMessage: "uninspected *.crate archive",
      },
      {
        name: "replace-with chain",
        config: `
[source.crates-io]
replace-with = "mirror"
[source.mirror]
replace-with = "vendor"
[source.vendor]
directory = "vendor"
`,
        writeSource(root: string) {
          writeFixtureFile(
            root,
            "server/vendor/helper/Cargo.toml",
            '[package]\nname = "helper"\n\n[dependencies]\nopentelemetry-otlp = "0.1"\n',
          );
        },
      },
      {
        name: "git-file source",
        config: `
[source.crates-io]
replace-with = "local"
[source.local]
git = "file:crates/waddle-xmpp-telemetry-helper"
`,
        writeSource(root: string) {
          writeSharedClientPathDependencyFixture(root);
        },
      },
    ];

    for (const shape of shapes) {
      withFixtureRoot((root) => {
        writeWasmFixture(root, { generatedJs: "export {};\n" });
        writeFixtureFile(root, "server/.cargo/config.toml", shape.config);
        shape.writeSource(root);

        const result = runChecker(["--wasm"], root);
        expect(result.status, shape.name).toBe(1);
        expect(result.stderr, shape.name).toContain(shape.expectMessage ?? "wasm dependency");
      });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic Cargo config patch is rejected in every native mode", () => {
    withFixtureRoot((root) => {
      writeAllPlatformFixtures(root);
      writeFixtureFile(
        root,
        "server/.cargo/config.toml",
        `
[patch.crates-io]
waddle-xmpp-telemetry-helper = { path = "crates/waddle-xmpp-telemetry-helper" }
`,
      );
      writeSharedClientPathDependencyFixture(root);

      expectRejectedInEveryNativeMode(root);
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic workspace-inherited local dependency is rejected in every native mode", () => {
    withFixtureRoot((root) => {
      writeAllPlatformFixtures(root);
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

      expectRejectedInEveryNativeMode(root);
    });
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
      withFixtureRoot((root) => {
        writeAllPlatformFixtures(root);
        writeSharedClientFixture(root, { clientCargoToml: shape.crate });
        writeFixtureFile(root, "server/Cargo.toml", shape.workspace);
        writeSharedClientPathDependencyFixture(root);

        expectRejectedInEveryNativeMode(root);
      });
    }
  }, CHECKER_TIMEOUT_MS);

  test("synthetic whitespace-padded TOML headers still reach closure discovery", () => {
    withFixtureRoot((root) => {
      writeAllPlatformFixtures(root);
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

      expectRejectedInEveryNativeMode(root);
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic quoted structural TOML headers still reach closure discovery", () => {
    withFixtureRoot((root) => {
      writeAllPlatformFixtures(root);
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

      expectRejectedInEveryNativeMode(root);
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic escaped TOML path still reaches closure discovery", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic dev-only local dependency is excluded from the product closure", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("malformed Cargo manifest in the local closure fails closed", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client exporter init is rejected in apple mode", () => {
    withFixtureRoot((root) => {
      writeAppleFixture(root);
      writeFixtureFile(
        root,
        "server/crates/waddle-xmpp-core/src/lib.rs",
        "pub fn install_exporter() { tracing_subscriber::fmt().init(); }\n",
      );

      const result = runChecker(["--apple"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("apple exporter");
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client lib target outside src is rejected", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic shared client build script collector is rejected", () => {
    withFixtureRoot((root) => {
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
    });
  }, CHECKER_TIMEOUT_MS);

  test("synthetic generated wasm glue with a collector beacon is rejected", () => {
    withFixtureRoot((root) => {
      const gitInit = spawnSync("git", ["init", "--quiet", root], {
        encoding: "utf8",
      });
      expect(gitInit.status).toBe(0);
      writeFixtureFile(
        root,
        ".gitignore",
        "/server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js\n",
      );
      writeWasmFixture(root, {
        cargoToml: "[package]\nname = \"fixture\"\n",
        eventsSrc: "pub fn event() {}\n",
        generatedJs: 'fetch("https://telemetry.example/collect", { method: "POST" });\n',
      });

      // rg must bypass ignore files; environments without rg use grep, which
      // already scans ignored paths and therefore still exercises the contract.
      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm exporter");
    });
  });
});
