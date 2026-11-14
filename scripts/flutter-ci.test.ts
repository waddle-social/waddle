import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createContext, runInContext } from "node:vm";

const root = resolve(import.meta.dir, "..");
const temporary: string[] = [];
// Opt in with the reviewed Gradle 9 binary; runner-installed Gradle may differ.
const gradleBinary = process.env.WADDLE_GRADLE_BIN;
afterEach(() => {
  for (const path of temporary.splice(0)) rmSync(path, { recursive: true, force: true });
});

function harness(helper: string) {
  const directory = mkdtempSync(join(tmpdir(), "waddle-flutter-ci-"));
  temporary.push(directory);
  for (const path of ["scripts", "bin", "server/crates/waddle-xmpp-client-wasm", "apps/flutter/android", "sdk/ndk/28.2.13676358"])
    mkdirSync(join(directory, path), { recursive: true });
  const script = join(directory, "scripts", helper);
  copyFileSync(join(root, "scripts", helper), script);
  copyFileSync(join(root, "scripts/flutter-verify-apk.py"), join(directory, "scripts/flutter-verify-apk.py"));
  const record = join(directory, "calls.jsonl");
  for (const tool of ["cargo", "wasm-pack", "flutter", "dart", "python3"]) {
    writeFileSync(join(directory, "bin", tool), `#!/usr/bin/python3
import json, os, sys
with open(os.environ["FLUTTER_TEST_RECORD"], "a") as output:
    output.write(json.dumps({"tool": "${tool}", "args": sys.argv[1:], "cwd": os.getcwd(), "library": os.environ.get("WADDLE_FLUTTER_RUNTIME_LIBRARY")}) + "\\n")
if "${tool}" == "cargo" and sys.argv[1:2] == ["ndk"]:
    from pathlib import Path
    stage = Path(sys.argv[sys.argv.index("-o") + 1])
    for abi in ("arm64-v8a", "x86_64"):
        destination = stage / abi
        destination.mkdir(parents=True, exist_ok=True)
        if not (os.environ.get("FLUTTER_TEST_MISSING_RUNTIME") and abi == "x86_64"):
            (destination / "libwaddle_flutter_runtime.so").write_bytes(abi.encode())
        (destination / "libwaddle_xmpp_client_ffi.so").write_bytes(b"dependency-cdylib")
if "${tool}" == "flutter" and sys.argv[1:3] == ["build", "apk"]:
    from pathlib import Path
    from zipfile import ZipFile
    apk = Path("build/app/outputs/flutter-apk/app-debug.apk")
    apk.parent.mkdir(parents=True, exist_ok=True)
    with ZipFile(apk, "w") as output:
        output.writestr("lib/arm64-v8a/libwaddle_flutter_runtime.so", b"arm64")
        if not os.environ.get("FLUTTER_TEST_MISSING_ABI"):
            output.writestr("lib/x86_64/libwaddle_flutter_runtime.so", b"x64")
if "${tool}" == "python3" and sys.argv[1].endswith("flutter-verify-apk.py"):
    os.execv("/usr/bin/python3", ["/usr/bin/python3", *sys.argv[1:]])
sys.exit(int(os.environ.get("FLUTTER_TEST_EXIT", "0")))
`, { mode: 0o755 });
  }
  writeFileSync(join(directory, "bin/git"), `#!/bin/sh
case "$*" in
  'show -s --format=%ct HEAD') printf '%s\\n' 1800000000 ;;
  'rev-parse --short=12 HEAD') printf '%s\\n' abcdef123456 ;;
  *) exit 2 ;;
esac
`, { mode: 0o755 });
  const env = {
    ...process.env,
    PATH: `${directory}/bin:${process.env.PATH}`,
    ANDROID_HOME: join(directory, "sdk"),
    FLUTTER_TEST_RECORD: record,
  };
  const buildTools = join(directory, "sdk/build-tools/36.0.0");
  mkdirSync(buildTools, { recursive: true });
  writeFileSync(join(buildTools, "aapt"), `#!/usr/bin/python3
import os, sys
if sys.argv[-1].startswith("-"):
    raise SystemExit("Unexpected aapt option in place of APK")
package = "wrong.app" if os.environ.get("FLUTTER_TEST_BAD_PACKAGE") else "social.waddle.flutter"
print("package: name='%s' versionCode='30000000' versionName='pr-abcdef123456'" % package)
`, { mode: 0o755 });
  writeFileSync(join(buildTools, "apksigner"), `#!/usr/bin/python3
import hashlib, os, sys
if sys.argv[-1].startswith("-"):
    raise SystemExit("Unexpected apksigner option in place of APK")
digest = "0" * 64 if os.environ.get("FLUTTER_TEST_BAD_CERT") else hashlib.sha256(b"fixture-cert").hexdigest()
print("Signer #1 certificate SHA-256 digest: " + digest)
`, { mode: 0o755 });
  writeFileSync(join(directory, "bin/keytool"), "#!/bin/sh\nprintf fixture-cert\n", { mode: 0o755 });
  return {
    directory,
    run: (mode: string, extra: Record<string, string> = {}) =>
      Bun.spawnSync(["bash", script, mode], { env: { ...env, ...extra } }),
    verify: (apk: string) => Bun.spawnSync(
      ["/usr/bin/python3", join(directory, "scripts/flutter-verify-apk.py"), apk, "30000000", "pr-abcdef123456"],
      { cwd: directory, env },
    ),
    calls: () => readFileSync(record, "utf8").trim().split("\n").map(line => JSON.parse(line)),
  };
}

describe("standalone Rust build contracts", () => {
  test("cross-build targets both ABIs into the new app only", () => {
    const h = harness("build-flutter-rust.sh");
    for (const abi of ["arm64-v8a", "x86_64"]) {
      const output = join(h.directory, "apps/flutter/android/app/src/main/jniLibs", abi);
      mkdirSync(output, { recursive: true });
      writeFileSync(join(output, "libwaddle_xmpp_client_ffi.so"), "stale dependency");
    }
    expect(h.run("--android").exitCode).toBe(0);
    const [call] = h.calls();
    expect(call.cwd).toBe(join(h.directory, "server"));
    expect(call.args).toEqual([
      "ndk", "-t", "arm64-v8a", "-t", "x86_64", "--platform", "26", "-o",
      expect.any(String),
      "build", "--locked", "-p", "waddle-flutter-runtime", "--profile", "mobile",
    ]);
    expect(JSON.stringify(call)).not.toContain("apps/android");
    expect(JSON.stringify(call)).not.toContain("uniffi-bindgen");
    expect(existsSync(call.args[8])).toBe(false);
    for (const abi of ["arm64-v8a", "x86_64"]) {
      const output = join(h.directory, "apps/flutter/android/app/src/main/jniLibs", abi);
      expect(readdirSync(output)).toEqual(["libwaddle_flutter_runtime.so"]);
      expect(readFileSync(join(output, "libwaddle_flutter_runtime.so"), "utf8")).toBe(abi);
    }
  });
  test("failed or incomplete cargo-ndk output is never packaged and staging is cleaned", () => {
    const failures: Record<string, string>[] = [{ FLUTTER_TEST_EXIT: "19" }, { FLUTTER_TEST_MISSING_RUNTIME: "1" }];
    for (const failure of failures) {
      const h = harness("build-flutter-rust.sh");
      const result = h.run("--android", failure);
      expect(result.exitCode).toBe(failure.FLUTTER_TEST_EXIT ? 19 : 1);
      expect(existsSync(join(h.directory, "apps/flutter/android/app/src/main/jniLibs"))).toBe(false);
      expect(existsSync(h.calls()[0].args[8])).toBe(false);
    }
  });
  test("browser output is matched static web glue, independent of chat's bundler output", () => {
    const h = harness("build-flutter-rust.sh");
    expect(h.run("--web").exitCode).toBe(0);
    expect(h.calls()[0]).toMatchObject({
      tool: "wasm-pack",
      cwd: join(h.directory, "server/crates/waddle-xmpp-client-wasm"),
      args: ["build", "--target", "web", "--out-name", "waddle_xmpp_client_wasm", "--out-dir", join(h.directory, "apps/flutter/web/rust"), "--", "--locked"],
    });
  });
  test("host library and compiler failures retain their real exit status", () => {
    const h = harness("build-flutter-rust.sh");
    expect(h.run("--host", { FLUTTER_TEST_EXIT: "19" }).exitCode).toBe(19);
    expect(h.calls()[0].args).toEqual(["build", "--locked", "-p", "waddle-flutter-runtime"]);
  });
});

describe("standalone verification contracts", () => {
  test.skipIf(!gradleBinary)("SDK integration_test builds from a writable copy with Gradle 9", async () => {
    const directory = mkdtempSync(join(tmpdir(), "waddle-sdk-plugin-"));
    temporary.push(directory);
    const android = join(directory, "android");
    const sdkPlugin = join(directory, "immutable-sdk/integration_test/android");
    mkdirSync(android, { recursive: true });
    mkdirSync(sdkPlugin, { recursive: true });
    writeFileSync(join(sdkPlugin, "build.gradle.kts"), 'tasks.register("writeProbe") { doLast { projectDir.resolve("probe").writeText("ok") } }\n');
    chmodSync(sdkPlugin, 0o555);
    const settings = join(android, "settings.gradle.kts");
    const initial = `rootProject.name = "sdk-plugin-fixture"\ninclude(":integration_test")\nproject(":integration_test").projectDir = file(${JSON.stringify(sdkPlugin)})\n`;
    writeFileSync(settings, initial);
    const run = async () => {
      const process = Bun.spawn([gradleBinary!, "--offline", "--no-daemon", "-Dorg.gradle.vfs.watch=false", "-p", android, ":integration_test:writeProbe"], { stdout: "pipe", stderr: "pipe" });
      const [code, stdout, stderr] = await Promise.all([process.exited, new Response(process.stdout).text(), new Response(process.stderr).text()]);
      return { code, output: stdout + stderr };
    };
    try {
      const rejected = await run();
      expect(rejected.code).not.toBe(0);
      expect(rejected.output).toContain("can't be written to");
      copyFileSync(join(root, "apps/flutter/android/sdk-plugins.settings.gradle.kts"), join(android, "sdk-plugins.settings.gradle.kts"));
      writeFileSync(settings, initial + '\napply(from = "sdk-plugins.settings.gradle.kts")\n');
      const relocated = await run();
      expect(relocated.code).toBe(0);
      expect(readFileSync(join(directory, ".dart_tool/android-sdk-plugins/integration_test/probe"), "utf8")).toBe("ok");
      expect(Bun.file(join(sdkPlugin, "probe")).size).toBe(0);
    } finally {
      chmodSync(sdkPlugin, 0o755);
    }
  }, 60_000);
  test("APK command labels the alternative debug app and permits upgrades", () => {
    const h = harness("flutter-ci.sh");
    expect(h.run("apk").exitCode).toBe(0);
    expect(h.calls()[0].args).toEqual([
      "build", "apk", "--debug", "--no-pub", "--target-platform", "android-arm64,android-x64",
      "--build-number", "30000000", "--build-name", "pr-abcdef123456",
    ]);
    expect(readFileSync(join(h.directory, "apps/flutter/build/artifacts/waddle-flutter.apk"))).toEqual(readFileSync(join(h.directory, "apps/flutter/build/app/outputs/flutter-apk/app-debug.apk")));
  });
  test("wrong package, missing Rust ABI and wrong signer cannot be promoted for upload", () => {
    for (const failure of ["FLUTTER_TEST_BAD_PACKAGE", "FLUTTER_TEST_MISSING_ABI", "FLUTTER_TEST_BAD_CERT"]) {
      const h = harness("flutter-ci.sh");
      expect(h.run("apk", { [failure]: "1" }).exitCode).not.toBe(0);
      expect(Bun.file(join(h.directory, "apps/flutter/build/artifacts/waddle-flutter.apk")).size).toBe(0);
    }
  });
  test("an APK filename beginning with a dash is verified as a file, not an SDK option", () => {
    const h = harness("flutter-ci.sh");
    expect(h.run("apk").exitCode).toBe(0);
    renameSync(join(h.directory, "apps/flutter/build/artifacts/waddle-flutter.apk"), join(h.directory, "-preview.apk"));
    const verified = h.verify("-preview.apk");
    expect(verified.exitCode).toBe(0);
    expect(verified.stdout.toString()).toContain("Verified alternative Flutter package");
  });
  test("tests export real native-library and rendered-preview inputs and propagate failure", () => {
    const h = harness("flutter-ci.sh");
    expect(h.run("test", { FLUTTER_TEST_EXIT: "7" }).exitCode).toBe(7);
    expect(h.calls()[0].args[0]).toBe(`${h.directory}/server/crates/waddle-flutter-runtime/scripts/smoke_library.py`);
    expect(h.calls()[0].library).toBe(`${h.directory}/server/target/debug/libwaddle_flutter_runtime.so`);
    expect(h.run("test").exitCode).toBe(0);
    expect(h.calls().at(-1).args).toContain(`--dart-define=WADDLE_PREVIEW_DIR=${h.directory}/apps/flutter/build/design-previews`);
  });
  test("Rust checks fail closed on warnings and cover both native and WASM", () => {
    const h = harness("flutter-ci.sh");
    expect(h.run("rust").exitCode).toBe(0);
    const calls = h.calls();
    expect(calls).toHaveLength(6);
    expect(calls[0].args).toEqual([`${h.directory}/server/crates/waddle-flutter-runtime/scripts/generate_dispatch.py`, "--check"]);
    expect(calls[2].args).toEqual(["clippy", "--locked", "-p", "waddle-flutter-runtime", "-p", "waddle-flutter-timeline", "-p", "waddle-xmpp-client", "-p", "waddle-xmpp-client-ffi", "--features", "waddle-xmpp-client-ffi/serde", "--all-targets", "--", "-D", "warnings"]);
    expect(calls[3].args).toContain("waddle-client-session");
    expect(calls[3].args).toContain("waddle-flutter-timeline");
    expect(calls[3].args).toContain("--all-targets");
    expect(calls[4].args).toEqual(["test", "--locked", "-p", "waddle-xmpp-client-wasm", "--lib"]);
    expect(calls[5].args).toEqual(["clippy", "--locked", "-p", "waddle-xmpp-client-wasm", "--target", "wasm32-unknown-unknown", "--", "-D", "warnings"]);
  });
  test("the native-tree check accepts alternative-only changes and rejects a native edit", () => {
    const h = harness("flutter-boundary-check.sh");
    const execute = (...args: string[]) => {
      const result = Bun.spawnSync(args, { cwd: h.directory });
      expect(result.exitCode).toBe(0);
      return result.stdout.toString().trim();
    };
    // This fixture needs real git, rather than the command recorder used above.
    rmSync(join(h.directory, "bin/git"));
    mkdirSync(join(h.directory, "apps/android"), { recursive: true });
    mkdirSync(join(h.directory, "apps/flutter/lib"), { recursive: true });
    writeFileSync(join(h.directory, "apps/android/README.md"), "Native app\n");
    execute("git", "init", "--quiet");
    execute("git", "add", "apps/android/README.md");
    execute("git", "-c", "user.name=CI contract", "-c", "user.email=ci@example.org", "commit", "--quiet", "-m", "fixture");
    const base = execute("git", "rev-parse", "HEAD");
    writeFileSync(join(h.directory, "apps/flutter/lib/bridge.dart"), "// Direct Rust transport\n");
    expect(h.run("", { WADDLE_NATIVE_BASE_SHA: base }).exitCode).toBe(0);
    writeFileSync(join(h.directory, "apps/android/README.md"), "Unexpected native edit\n");
    expect(h.run("", { WADDLE_NATIVE_BASE_SHA: base }).exitCode).not.toBe(0);
  });
});

describe("browser push account boundary", () => {
  function browserContext() {
    const location = { href: "https://example.org/flutter/waddle_push.js?instance=old-install" };
    const context = createContext({ URL, location, self: { location, addEventListener() {} } });
    runInContext(readFileSync(join(root, "apps/flutter/web/waddle_push.js"), "utf8"), context);
    return context;
  }
  test("queued push from a previous account cannot notify the active one", () => {
    const context = browserContext();
    expect(runInContext("ownsWaddleRegistration({owner:'new@example.org',instance:'new-install',enabled:true}, 'old-install')", context)).toBe(false);
    expect(runInContext("ownsWaddleRegistration({owner:'old@example.org',instance:'old-install',enabled:false}, 'old-install')", context)).toBe(false);
    expect(runInContext("ownsWaddleRegistration({owner:'old@example.org',instance:'old-install',enabled:true}, 'old-install')", context)).toBe(true);
  });
  test("worker refuses malformed conversation/resource/class envelopes", () => {
    const context = browserContext();
    for (const conversation of ["", "a@@example.org", "a@example.org/private", "a\n@example.org"]) {
      context.envelope = { v: 1, class: "dm", conversation, item: "id" };
      expect(runInContext("parseWaddlePush(envelope)", context)).toBeNull();
    }
    context.envelope = { v: 1, class: "dm", conversation: "a@example.org", item: "id" };
    expect(runInContext("parseWaddlePush(envelope).conversation", context)).toBe("a@example.org");
  });
});

describe("artifact and aggregate quality gates", () => {
  const workflow = Bun.YAML.parse(readFileSync(join(root, ".github/workflows/waddle-flutter-apk.yml"), "utf8"));
  const job = workflow.jobs.flutter;
  const step = (id: string) => job.steps.find((value: { id: string }) => value.id === id);
  test("builds remain independent of quality failures without suppressing them", () => {
    expect(job["runs-on"]).toBe("ubuntu-24.04");
    expect(workflow.permissions).toEqual({ contents: "read" });
    expect(job.steps.some((value: Record<string, unknown>) => value["continue-on-error"])).toBe(false);
    expect(step("apk").if).toContain("!cancelled()");
    expect(step("apk").if).not.toMatch(/analyze|tests|rust/);
    expect(step("native").if).not.toMatch(/analyze|tests|rust/);
    expect(step("apk_artifact").if).toContain("always()");
    expect(step("apk_artifact").with).toMatchObject({ name: "waddle-flutter-apk", "if-no-files-found": "error", "retention-days": 7 });
    expect(step("preview_artifact").if).toContain("always()");
    expect(step("smoke").if).toContain("steps.apk.outcome == 'success'");
    expect(step("smoke_artifact").if).toContain("always()");
    expect(step("browser").run).toContain("WADDLE_REQUIRE_BROWSER_CONTRACT=1");
    expect(job.steps.filter((value: { uses?: string }) => value.uses?.startsWith("actions/upload-artifact@")).every((value: { with: { path: string } }) => value.with.path.startsWith("apps/flutter/"))).toBe(true);
  });
  test("the existing aggregate still rejects a failed standalone lane despite its APK", async () => {
    const { readLanes, generate, verifyGate } = await import("./integration-ci");
    const lane = readLanes().find(value => value.file.endsWith("waddle-flutter-apk.yml"));
    expect(lane).toBeDefined();
    expect(generate([lane!])).toContain("waddle_flutter_apk__flutter");
    const id = "waddle_flutter_apk__flutter";
    expect(() => verifyGate({ plan: { result: "success" }, [id]: { result: "failure" } }, [id], [id])).toThrow();
    expect(() => verifyGate({ plan: { result: "success" }, [id]: { result: "success" } }, [id], [id])).not.toThrow();
  });
});

describe("Android smoke screenshot handshake", () => {
  const smoke = readFileSync(join(root, "scripts/flutter-device-smoke.sh"), "utf8");
  const capture = smoke.slice(smoke.indexOf("capture_production_ui() {"), smoke.indexOf("\ncleanup() {"));
  const cleanup = smoke.slice(smoke.indexOf("cleanup() {"), smoke.indexOf("\ntrap cleanup EXIT"));
  const token = "0123456789abcdef0123456789abcdef";
  function fixture(mode: string) {
    const directory = mkdtempSync(join(tmpdir(), "waddle-smoke-capture-"));
    temporary.push(directory);
    const artifact = join(directory, "artifacts");
    mkdirSync(artifact);
    mkdirSync(join(directory, "temp"));
    const adb = join(directory, "adb");
    writeFileSync(adb, `#!/usr/bin/python3
import base64, json, os, sys
from pathlib import Path
root = Path(os.environ["SMOKE_FIXTURE"])
mode = os.environ["SMOKE_MODE"]
args = sys.argv[1:]
assert args[:2] == ["-s", "emulator-5554"], args
args = args[2:]
with (root / "calls").open("a") as log:
    log.write(json.dumps(args) + "\\n")
if args == ["exec-out", "run-as", "social.waddle.flutter", "cat", "cache/standalone-smoke.ready"]:
    if mode == "missing": sys.exit(1)
    probe = root / "probe"
    count = int(probe.read_text()) + 1 if probe.exists() else 1
    probe.write_text(str(count))
    print("stale-run-token" if mode == "stale" or (mode == "delayed" and count == 1) else os.environ["SMOKE_CAPTURE_TOKEN"], end="")
elif args == ["exec-out", "screencap", "-p"]:
    if mode == "capture-failed": sys.exit(17)
    image = b"not a PNG" if mode == "invalid" else base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII=")
    sys.stdout.buffer.write(image)
elif args == ["exec-in", "run-as", "social.waddle.flutter", "tee", "cache/standalone-smoke.ack"]:
    assert (root / "artifacts/production.png").read_bytes().startswith(b"\\x89PNG\\r\\n\\x1a\\n")
    value = sys.stdin.read()
    assert value == os.environ["SMOKE_CAPTURE_TOKEN"], value
    if mode == "ack-failed": sys.exit(23)
    (root / "ack").write_text(value)
else: raise AssertionError(args)
`, { mode: 0o755 });
    const env = { ...process.env, ADB: adb, SERIAL: "emulator-5554", SMOKE_ARTIFACT: artifact, SMOKE_CAPTURE_TOKEN: token, SMOKE_FIXTURE: directory, SMOKE_MODE: mode };
    return { directory, artifact, env, run: () => Bun.spawnSync(["bash", "-c", `set -euo pipefail\n${capture}\ncapture_production_ui 2`], { env }) };
  }
  test("captures only the current ready token and acknowledges after promoting a valid PNG", () => {
    const h = fixture("delayed");
    const result = h.run();
    expect(result.exitCode).toBe(0);
    expect(readFileSync(join(h.directory, "probe"), "utf8")).toBe("2");
    expect(readFileSync(join(h.directory, "ack"), "utf8")).toBe(token);
    expect(existsSync(join(h.artifact, "production.png"))).toBe(true);
    expect(existsSync(join(h.artifact, "production.pending.png"))).toBe(false);
  });
  test("capture failure or invalid PNG cannot acknowledge or publish production evidence", () => {
    for (const mode of ["capture-failed", "invalid"]) {
      const h = fixture(mode);
      expect(h.run().exitCode).not.toBe(0);
      expect(existsSync(join(h.directory, "ack"))).toBe(false);
      expect(existsSync(join(h.artifact, "production.png"))).toBe(false);
    }
  });
  test("an old run token expires without capturing and acknowledgment errors remain failures", () => {
    const stale = fixture("stale");
    expect(stale.run().exitCode).toBe(1);
    expect(existsSync(join(stale.artifact, "production.png"))).toBe(false);
    expect(existsSync(join(stale.directory, "ack"))).toBe(false);
    const failed = fixture("ack-failed");
    expect(failed.run().exitCode).toBe(23);
    expect(existsSync(join(failed.directory, "ack"))).toBe(false);
  });
  test("parent cleanup retires the capture poller and preserves the Flutter failure status", () => {
    const h = fixture("missing");
    const result = Bun.spawnSync(["bash", "-c", `set -euo pipefail
${capture}
${cleanup}
SMOKE_TEMP="$SMOKE_FIXTURE/temp"
SMOKE_LOG="$SMOKE_TEMP/emulator.log"
EMULATOR_PID=''
SMOKE_BOOTED=false
CAPTURE_PID=''
trap cleanup EXIT
capture_production_ui 60 &
CAPTURE_PID=$!
printf '%s' "$CAPTURE_PID" > "$SMOKE_FIXTURE/poller.pid"
fixture_deadline=$((SECONDS + 3))
while [[ ! -f "$SMOKE_FIXTURE/calls" && $SECONDS -lt $fixture_deadline ]]; do sleep 0.02; done
test -f "$SMOKE_FIXTURE/calls"
exit 7
`], { env: h.env });
    expect(result.exitCode).toBe(7);
    expect(readFileSync(join(h.artifact, "status.txt"), "utf8")).toBe("exit_status=7\n");
    const pid = readFileSync(join(h.directory, "poller.pid"), "utf8");
    expect(Bun.spawnSync(["kill", "-0", pid]).exitCode).not.toBe(0);
  });
});
