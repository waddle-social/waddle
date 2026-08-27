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

describe("native remote telemetry contract", () => {
  test("WASM/browser surfaces remain free of remote telemetry exporters", () => {
    const result = runChecker(["--wasm"]);
    expect(result.status).toBe(0);
    expect(result.stdout.trim()).toContain("native remote telemetry contract OK: wasm");
  });

  test("checker validates every native surface in one pass", () => {
    const result = runChecker(["--all"]);
    expect(result.status).toBe(0);
    expect(result.stdout.trim()).toContain("native remote telemetry contract OK: all");
  });

  test("checker rejects unknown modes", () => {
    const result = runChecker(["--bogus"]);
    expect(result.status).toBe(2);
    expect(result.stderr).toContain("--apple|--android|--wasm|--all");
  });

  test("synthetic wasm fixture fails when a remote telemetry subscriber is added", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      mkdirSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/src"), { recursive: true });
      writeFileSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/Cargo.toml"), `
[package]
name = "waddle-xmpp-client-wasm"

[dependencies]
tracing = "0.1"
tracing-subscriber = "0.3"
`);
      writeFileSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/src/events.rs"), "pub fn local_callback() {}\n");
      writeFileSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/src/driver.rs"), "pub fn driver() {}\n");

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("synthetic android fixture fails when crashlytics is added", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      mkdirSync(resolve(root, "apps/android/app"), { recursive: true });
      mkdirSync(resolve(root, "apps/android/core/client"), { recursive: true });
      writeFileSync(resolve(root, "apps/android/app/build.gradle.kts"), 'plugins { id("com.google.firebase.crashlytics") }\n');
      writeFileSync(resolve(root, "apps/android/core/client/build.gradle.kts"), "plugins {}\n");

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android dependency");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("synthetic apple bootstrap outside the known app files is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      mkdirSync(resolve(root, "apps/apple/Waddle/Support"), { recursive: true });
      mkdirSync(resolve(root, "apps/apple/Waddle/App"), { recursive: true });
      mkdirSync(resolve(root, "apps/apple/Waddle/RustClient"), { recursive: true });
      mkdirSync(resolve(root, "apps/apple/Waddle.xcodeproj"), { recursive: true });
      writeFileSync(resolve(root, "apps/apple/project.yml"), "name: Waddle\n");
      writeFileSync(resolve(root, "apps/apple/Waddle.xcodeproj/project.pbxproj"), "// project\n");
      writeFileSync(resolve(root, "apps/apple/Waddle/App/AppModel.swift"), "import OSLog\n");
      writeFileSync(resolve(root, "apps/apple/Waddle/RustClient/RustXmppClient.swift"), "import OSLog\n");
      writeFileSync(
        resolve(root, "apps/apple/Waddle/Support/TelemetryBootstrap.swift"),
        'let collectorURL = "https://telemetry.example/v1/traces"\n',
      );

      const result = runChecker(["--apple"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("apple exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("synthetic generic collector configuration is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      mkdirSync(resolve(root, "apps/android/app"), { recursive: true });
      mkdirSync(resolve(root, "apps/android/core/client"), { recursive: true });
      writeFileSync(resolve(root, "apps/android/app/build.gradle.kts"), "plugins {}\n");
      writeFileSync(resolve(root, "apps/android/core/client/build.gradle.kts"), "plugins {}\n");
      writeFileSync(
        resolve(root, "apps/android/gradle.properties"),
        "COLLECTOR_URL=https://telemetry.example/collect\n",
      );

      const result = runChecker(["--android"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("android exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("synthetic generated wasm glue with a collector beacon is rejected", () => {
    const root = mkdtempSync(resolve(tmpdir(), "waddle-native-telemetry-"));
    try {
      mkdirSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/src"), { recursive: true });
      mkdirSync(resolve(root, "server/wasm-pkg/waddle-xmpp-client-wasm"), { recursive: true });
      writeFileSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/Cargo.toml"), "[package]\nname = \"fixture\"\n");
      writeFileSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/src/events.rs"), "pub fn event() {}\n");
      writeFileSync(resolve(root, "server/crates/waddle-xmpp-client-wasm/src/driver.rs"), "pub fn driver() {}\n");
      writeFileSync(
        resolve(root, "server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js"),
        'fetch("https://telemetry.example/collect", { method: "POST" });\n',
      );

      const result = runChecker(["--wasm"], root);
      expect(result.status).toBe(1);
      expect(result.stderr).toContain("wasm exporter");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
