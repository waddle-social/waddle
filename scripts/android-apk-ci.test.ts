import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { hostedApkWorkflow, outputFile, render, sourceFile } from "./android-apk-ci";

const source = () => Bun.YAML.parse(readFileSync(sourceFile, "utf8")) as Record<string, any>;

test("hosted APK uses the unchanged Android build and validation steps", () => {
  const original = source();
  const workflow = hostedApkWorkflow(original);
  const sourceJob = Object.values(original.jobs)[0] as Record<string, any>;
  expect(workflow.jobs.apk.steps).toEqual(sourceJob.steps.filter((step: any) =>
    !step.uses?.startsWith("namespacelabs/nscloud-cache-action@")));
  expect(workflow.jobs.apk.env).toEqual(sourceJob.env);
  expect(workflow.jobs.apk.permissions).toEqual(sourceJob.permissions);
  expect(workflow.jobs.apk["runs-on"]).toBe("ubuntu-24.04");
  expect(workflow.concurrency.group).not.toBe(original.concurrency.group);
  expect(original.jobs).not.toHaveProperty("apk");
  expect(workflow.jobs.apk.steps.some((step: any) => step["continue-on-error"])).toBe(false);
});

test("both check outcomes upload the exact signed FOSS APK and reject missing files", () => {
  const steps = hostedApkWorkflow(source()).jobs.apk.steps;
  for (const condition of ["success()", "failure()"]) {
    const upload = steps.find((step: any) => step.name.startsWith("Upload Android APK") && step.if === condition);
    expect(upload.with.name).toBe("waddle-android-apk");
    expect(upload.with.path).toBe("apps/android/app/build/outputs/apk/foss/debug/app-foss-debug.apk");
    expect(upload.with["if-no-files-found"]).toBe("error");
  }
});

test("unexpected source runners or graph shapes require a reviewed update", () => {
  const changed = source();
  (Object.values(changed.jobs)[0] as any)["runs-on"] = "other-runner";
  expect(() => hostedApkWorkflow(changed)).toThrow();
  expect(() => hostedApkWorkflow({ jobs: {} })).toThrow();
});

test("checked-in workflow exactly matches its Android source", () => {
  expect(readFileSync(outputFile, "utf8")).toBe(render(source()));
});
