import { readFileSync, writeFileSync } from "node:fs";

export const sourceFile = ".github/workflows/waddle-android-pullrequest.yml";
export const outputFile = ".github/workflows/waddle-android-apk.yml";

// Use the same build and verification graph on an independent runner pool.
// The Namespace volume action is specific to its hosts; all other steps remain.
export function hostedApkWorkflow(source: Record<string, any>): Record<string, any> {
  const jobs = Object.values(source.jobs ?? {}) as Record<string, any>[];
  if (jobs.length !== 1 || jobs[0]["runs-on"] !== "namespace-profile-linux-x86") {
    throw new Error("Review the Android source job before changing its hosted derivative");
  }
  const workflow = structuredClone(source);
  const job = structuredClone(jobs[0]);
  const namespaceSteps = job.steps.filter((step: any) =>
    step.uses?.startsWith("namespacelabs/nscloud-cache-action@"));
  if (namespaceSteps.length !== 1) throw new Error("Expected exactly one Namespace cache step");
  job.steps = job.steps.filter((step: any) =>
    !step.uses?.startsWith("namespacelabs/nscloud-cache-action@"));
  job["runs-on"] = "ubuntu-24.04";
  job["timeout-minutes"] = 90;
  job.name = "Build downloadable Android APK";
  workflow.name = "waddle-android-apk";
  workflow.jobs = { apk: job };
  workflow.concurrency = {
    group: "android-apk-${{ github.head_ref || github.ref }}",
    "cancel-in-progress": true,
  };
  workflow.on.pull_request.paths.push("scripts/android-apk-ci.ts", "scripts/android-apk-ci.test.ts");
  return workflow;
}

export function render(source: Record<string, any>): string {
  return "# Generated from the Android PR workflow; do not edit manually.\n" +
    "# Regenerate with: bun scripts/android-apk-ci.ts sync\n\n" +
    Bun.YAML.stringify(hostedApkWorkflow(source), null, 2).replace(/[ \t]+$/gm, "").trimEnd() + "\n";
}

if (import.meta.main) {
  const expected = render(Bun.YAML.parse(readFileSync(sourceFile, "utf8")) as Record<string, any>);
  if (process.argv.includes("sync")) {
    writeFileSync(outputFile, expected);
    console.log(`Generated ${outputFile}`);
  } else if (readFileSync(outputFile, "utf8") !== expected) {
    throw new Error("Android APK workflow drift; run bun scripts/android-apk-ci.ts sync");
  }
}
