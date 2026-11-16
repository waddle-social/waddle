import { describe, expect, test } from "bun:test";
import { aggregate, classify, durationMinutes, fetchJobs, pages, parseRates, run, toRecords, withRetry, type Job } from "./ci-cost-report";

const NS32 = "nscloud-ubuntu-24.04-amd64-32x64-with-features";
const job = (o: Partial<Job>): Job => ({ name: "build", workflow_name: "ci", status: "completed", conclusion: "success", labels: ["namespace-profile-linux-x86"], started_at: "2026-01-01T00:00:00Z", completed_at: "2026-01-01T00:10:00Z", event: "pull_request", ...o });

describe("classify", () => {
  test("example labels", () => {
    expect(classify(["ubuntu-24.04"])).toEqual({ cls: "github-hosted", vcpu: null, weighted: false });
    expect(classify(["namespace-profile-linux-x86"])).toMatchObject({ cls: "namespace-linux", vcpu: 8 });
    expect(classify(["namespace-profile-linux-x86"], 16).vcpu).toBe(16);
    expect(classify(["namespace-profile-darwin-arm64"])).toMatchObject({ cls: "namespace-macos", vcpu: null });
    expect(classify([NS32])).toMatchObject({ cls: "namespace-linux-32x64", vcpu: 32 });
  });
  test("two-label arrays and unknown", () => {
    expect(classify([NS32, "namespace-features:container.privileged=true;job.priority=1"]).cls).toBe("namespace-linux-32x64");
    expect(classify(["weird-runner"]).cls).toBe("unknown:weird-runner");
  });
});

describe("durations", () => {
  test("computes minutes and skips never-started jobs", () => {
    expect(durationMinutes(job({}))).toBe(10);
    expect(durationMinutes(job({ started_at: null }))).toBeNull();
    expect(durationMinutes(job({ completed_at: null }))).toBeNull();
    const recs = toRecords([job({}), job({ started_at: null, conclusion: "skipped" }), job({ started_at: "0001-01-01T00:00:00Z", completed_at: "0001-01-01T00:00:00Z" })]);
    expect(recs.length).toBe(2);
  });
});

describe("aggregate", () => {
  const jobs = [
    job({}),
    job({ conclusion: "cancelled", completed_at: "2026-01-01T00:05:00Z" }),
    job({ name: "lint", labels: [NS32], conclusion: "failure", event: "push", completed_at: "2026-01-01T00:02:00Z" }),
    job({ name: "gh", labels: ["ubuntu-24.04"], event: "push" }),
  ];
  const rep = aggregate(toRecords(jobs));
  test("units and shares", () => {
    // 10*8 + 5*8 + 2*32 = 184
    expect(rep.totalUnits).toBe(184);
    expect(rep.classes.find((c) => c.cls === "github-hosted")).toMatchObject({ minutes: 10, units: 0, share: 0 });
    const build = rep.jobs.find((j) => j.job === "build")!;
    expect(build).toMatchObject({ jobs: 2, minutes: 15, units: 120 });
    expect(build.share).toBeCloseTo(120 / 184);
    expect(build.split).toMatchObject({ success: 10, cancelled: 5, failure: 0 });
    expect(rep.jobs.find((j) => j.job === "lint")!.split.failure).toBe(2);
  });
  test("cancelled attribution and events", () => {
    expect(rep.cancelled).toEqual([expect.objectContaining({ workflow: "ci", jobs: 1, minutes: 5, units: 40 })]);
    expect(rep.events.find((e) => e.event === "push")!.minutes).toBe(12);
  });
  test("top limit and rates", () => {
    expect(aggregate(toRecords(jobs), { top: 1 }).jobs.length).toBe(1);
    const c = aggregate(toRecords(jobs), { rates: parseRates(["namespace-linux=0.01"]) }).classes.find((x) => x.cls === "namespace-linux")!;
    expect(c.cost).toBeCloseTo(0.15);
    expect(() => parseRates(["bad"])).toThrow();
  });
});

describe("--stdin end to end", () => {
  const jobs = [job({}), job({ name: "mac", labels: ["namespace-profile-darwin-arm64"], conclusion: "cancelled" })];
  test("array and paged shapes", async () => {
    const a = JSON.stringify(jobs), b = JSON.stringify([{ total_count: 1, jobs: [jobs[0]] }, { jobs: [jobs[1]] }]);
    for (const input of [a, b]) {
      const text = await run(["--stdin"], {}, async () => input);
      expect(text).toContain("assumed 8 vCPU");
      expect(text).toContain("namespace-macos");
      expect(text).toContain("Cancelled spend by workflow");
    }
    const json = JSON.parse(await run(["--stdin", "--json", "--profile-vcpu", "4"], {}, async () => a));
    expect(json.totalUnits).toBe(10 * 4 + 10);
  });
  test("requires a token without --stdin", async () => {
    await expect(run([], {})).rejects.toThrow(/GITHUB_TOKEN/);
  });
});

describe("paging", () => {
  test("builds page URLs itself and stops on a short page", async () => {
    const urls: string[] = [];
    const get = async (u: string) => { urls.push(u); const p = Number(u.match(/&page=(\d+)/)![1]); return { total_count: 5, jobs: Array(p < 3 ? 2 : 1).fill({ n: p }) }; };
    const r = await pages("https://x/jobs?per_page=2", get, (b) => b.jobs, 2);
    expect(r.items.length).toBe(5);
    expect(urls).toEqual(["https://x/jobs?per_page=2&page=1", "https://x/jobs?per_page=2&page=2", "https://x/jobs?per_page=2&page=3"]);
  });
  test("fetchJobs lists runs then jobs with injected get", async () => {
    const urls: string[] = [];
    const get = async (u: string) => {
      urls.push(u);
      if (u.includes("/actions/runs?")) return { total_count: 1, workflow_runs: u.includes("page=1") ? [{ id: 7, event: "push", name: "CI" }] : [] };
      return { jobs: [job({ event: undefined })] };
    };
    const jobs = await fetchJobs("o/r", 1, "t", 2, get);
    expect(jobs.length).toBe(1);
    expect(jobs[0]).toMatchObject({ event: "push", run_name: "CI" });
    expect(urls.every((u) => u.startsWith("https://api.github.com/repos/o/r/"))).toBe(true);
  });
});

describe("retry", () => {
  test("retries 5xx and network errors up to 3 attempts", async () => {
    let calls = 0;
    const flaky = async () => { if (++calls < 3) throw calls === 1 ? Object.assign(new Error("502"), { status: 502 }) : new TypeError("network"); return "ok"; };
    expect(await withRetry(flaky, 3, 1)("u")).toBe("ok");
    expect(calls).toBe(3);
    let n = 0;
    const down = async () => { n++; throw Object.assign(new Error("502"), { status: 502 }); };
    await expect(withRetry(down, 3, 1)("u")).rejects.toThrow("502");
    expect(n).toBe(3);
  });
  test("does not retry 4xx", async () => {
    let n = 0;
    await expect(withRetry(async () => { n++; throw Object.assign(new Error("404"), { status: 404 }); }, 3, 1)("u")).rejects.toThrow();
    expect(n).toBe(1);
  });
  test("fetchJobs recovers from a transient failure", async () => {
    let failed = false;
    const get = async (u: string) => {
      if (u.includes("/jobs") && !failed) { failed = true; throw Object.assign(new Error("502"), { status: 502 }); }
      return u.includes("/actions/runs?") ? { workflow_runs: u.includes("&page=1") ? [{ id: 1, event: "push", name: "CI" }] : [] } : { jobs: [job({})] };
    };
    expect((await fetchJobs("o/r", 1, "t", 4, get, 1)).length).toBe(1);
  });
});
