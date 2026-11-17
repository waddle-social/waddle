import { parseArgs } from "node:util";

// Attributes GitHub Actions runner-minutes (mostly Namespace) to workflows/jobs.
// Units: vCPU-minutes for Namespace Linux, plain minutes for macOS/unknown, 0 for GitHub-hosted.

export type Job = {
  id?: number; run_id?: number; name: string; workflow_name?: string | null; status?: string;
  conclusion?: string | null; labels?: string[]; started_at?: string | null; completed_at?: string | null;
  head_branch?: string | null; event?: string; run_name?: string;
};
export type Runner = { cls: string; vcpu: number | null; weighted: boolean };
export type Rec = { workflow: string; job: string; event: string; runner: Runner; minutes: number; conclusion: string };
type Bucket = { jobs: number; minutes: number; units: number };

const githubHosted = /^(ubuntu|windows|macos)-[\w.-]+$/;

export function classify(labels: string[] = [], profileVcpu = 8): Runner {
  for (const label of labels) {
    if (label.startsWith("nscloud-")) {
      const m = label.match(/-(\d+)x(\d+)/);
      if (m) return { cls: `namespace-linux-${m[1]}x${m[2]}`, vcpu: Number(m[1]), weighted: true };
    }
    if (label === "namespace-profile-linux-x86") return { cls: "namespace-linux", vcpu: profileVcpu, weighted: true };
    if (label === "namespace-profile-darwin-arm64") return { cls: "namespace-macos", vcpu: null, weighted: true };
  }
  const hosted = labels.find((l) => githubHosted.test(l));
  if (hosted) return { cls: "github-hosted", vcpu: null, weighted: false };
  return { cls: `unknown:${labels[0] ?? ""}`, vcpu: null, weighted: true };
}

/** Minutes between start and completion; null when the job never started or finished. */
export function durationMinutes(job: Pick<Job, "started_at" | "completed_at">): number | null {
  if (!job.started_at || !job.completed_at) return null;
  const ms = Date.parse(job.completed_at) - Date.parse(job.started_at);
  return Number.isFinite(ms) && ms >= 0 ? ms / 60000 : null;
}

export const units = (r: Runner, minutes: number) => !r.weighted ? 0 : minutes * (r.vcpu ?? 1);

export function toRecords(jobs: Job[], profileVcpu = 8): Rec[] {
  const out: Rec[] = [];
  for (const j of jobs) {
    const minutes = durationMinutes(j);
    if (minutes === null) continue;
    out.push({
      workflow: j.workflow_name ?? j.run_name ?? "(unknown workflow)", job: j.name, event: j.event ?? "unknown",
      runner: classify(j.labels, profileVcpu), minutes, conclusion: j.conclusion ?? j.status ?? "unknown",
    });
  }
  return out;
}

const conclusionKey = (c: string) =>
  c === "success" ? "success" : c === "cancelled" ? "cancelled" : ["failure", "timed_out", "startup_failure"].includes(c) ? "failure" : "other";

const add = (b: Bucket, r: Rec) => { b.jobs++; b.minutes += r.minutes; b.units += units(r.runner, r.minutes); };
const bucket = (): Bucket => ({ jobs: 0, minutes: 0, units: 0 });
function group<T>(recs: Rec[], key: (r: Rec) => string, init: () => T, fold: (t: T, r: Rec) => void) {
  const m = new Map<string, T>();
  for (const r of recs) { const k = key(r); if (!m.has(k)) m.set(k, init()); fold(m.get(k)!, r); }
  return m;
}
const byUnits = (a: { units: number; minutes: number }, b: { units: number; minutes: number }) => b.units - a.units || b.minutes - a.minutes;

export function aggregate(recs: Rec[], opts: { top?: number; rates?: Record<string, number> } = {}) {
  const { top = 25, rates = {} } = opts;
  const total = recs.reduce((s, r) => s + units(r.runner, r.minutes), 0);
  const share = (u: number) => (total ? u / total : 0);
  const cost = (cls: string, minutes: number) => (cls in rates ? minutes * rates[cls] : undefined);

  const classes = [...group(recs, (r) => r.runner.cls, bucket, add)].map(([cls, b]) => ({ cls, ...b, share: share(b.units), cost: cost(cls, b.minutes) })).sort(byUnits);

  type Row = Bucket & { workflow: string; job: string; cls: string; split: Record<string, number> };
  const jobs = [...group<Row>(recs, (r) => `${r.workflow}\u0000${r.job}`, () => ({ ...bucket(), workflow: "", job: "", cls: "", split: { success: 0, failure: 0, cancelled: 0, other: 0 } }), (row, r) => {
    add(row, r); row.workflow = r.workflow; row.job = r.job; row.cls = r.runner.cls; row.split[conclusionKey(r.conclusion)] += r.minutes;
  }).values()].map((row) => ({ ...row, share: share(row.units) })).sort(byUnits).slice(0, top);

  const cancelled = [...group(recs.filter((r) => r.conclusion === "cancelled"), (r) => r.workflow, bucket, add)]
    .map(([workflow, b]) => ({ workflow, ...b, share: share(b.units) })).sort(byUnits);
  const events = [...group(recs, (r) => r.event, bucket, add)].map(([event, b]) => ({ event, ...b, share: share(b.units) })).sort(byUnits);

  return { totalUnits: total, totalMinutes: recs.reduce((s, r) => s + r.minutes, 0), classes, jobs, cancelled, events };
}
export type Report = ReturnType<typeof aggregate>;

const n = (x: number) => x.toFixed(1);
const pct = (x: number) => `${(x * 100).toFixed(1)}%`;
function table(head: string[], rows: string[][]): string {
  const w = head.map((h, i) => Math.max(h.length, ...rows.map((r) => r[i].length)));
  const line = (r: string[]) => r.map((c, i) => (i === 0 ? c.padEnd(w[i]) : c.padStart(w[i]))).join("  ");
  return [line(head), ...rows.map(line)].join("\n");
}

export function format(rep: Report, meta: { profileVcpu: number; label: string; rates?: Record<string, number> }): string {
  const hasCost = Object.keys(meta.rates ?? {}).length > 0;
  const usd = (c?: number) => (c === undefined ? "-" : `$${c.toFixed(2)}`);
  const out = [
    `CI cost report: ${meta.label}`,
    `Units: vCPU-minutes for Namespace Linux (namespace-profile-linux-x86 assumed ${meta.profileVcpu} vCPU, see --profile-vcpu); plain minutes for macOS/unknown; GitHub-hosted weighted 0.`,
    `Total: ${n(rep.totalMinutes)} min, ${n(rep.totalUnits)} units`,
    "",
    "By runner class",
    table(["class", "jobs", "min", "units", "share", ...(hasCost ? ["cost"] : [])],
      rep.classes.map((c) => [c.cls, `${c.jobs}`, n(c.minutes), n(c.units), pct(c.share), ...(hasCost ? [usd(c.cost)] : [])])),
    "", `Top ${rep.jobs.length} by workflow / job (minutes split by conclusion)`,
    table(["workflow / job [class]", "runs", "min", "units", "share", "success", "failure", "cancelled"],
      rep.jobs.map((j) => [`${j.workflow} / ${j.job} [${j.cls}]`, `${j.jobs}`, n(j.minutes), n(j.units), pct(j.share), n(j.split.success), n(j.split.failure), n(j.split.cancelled)])),
    "", "Cancelled spend by workflow",
    rep.cancelled.length ? table(["workflow", "jobs", "min", "units", "share"], rep.cancelled.map((c) => [c.workflow, `${c.jobs}`, n(c.minutes), n(c.units), pct(c.share)])) : "(none)",
    "", "By event",
    table(["event", "jobs", "min", "units", "share"], rep.events.map((e) => [e.event, `${e.jobs}`, n(e.minutes), n(e.units), pct(e.share)])),
  ];
  return out.join("\n") + "\n";
}

export function parseRates(values: string[] = []): Record<string, number> {
  const rates: Record<string, number> = {};
  for (const v of values) {
    const i = v.lastIndexOf("=");
    const rate = Number(v.slice(i + 1));
    if (i < 1 || !Number.isFinite(rate) || rate < 0) throw new Error(`Invalid --rate ${v}; expected <class>=<usd-per-minute>`);
    rates[v.slice(0, i)] = rate;
  }
  return rates;
}

/** Accepts an array of jobs, `{jobs}` pages, or an array of such pages. */
export function parseJobs(text: string): Job[] {
  const data = JSON.parse(text);
  const pages: any[] = Array.isArray(data) ? data : [data];
  return pages.flatMap((p) => (Array.isArray(p?.jobs) ? p.jobs : [p]));
}

// ---- GitHub IO ----

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function gh(url: string, token: string, attempt = 0): Promise<Response> {
  const res = await fetch(url, { headers: { authorization: `Bearer ${token}`, accept: "application/vnd.github+json", "x-github-api-version": "2022-11-28" } });
  const limited = res.status === 429 || (res.status === 403 && (res.headers.has("retry-after") || res.headers.get("x-ratelimit-remaining") === "0"));
  if (limited && attempt < 5) {
    const retry = Number(res.headers.get("retry-after"));
    const reset = Number(res.headers.get("x-ratelimit-reset"));
    const waitMs = retry ? retry * 1000 : reset ? Math.max(reset * 1000 - Date.now(), 0) + 1000 : 2 ** attempt * 1000;
    console.error(`rate limited; waiting ${Math.ceil(waitMs / 1000)}s`);
    await sleep(Math.min(waitMs, 15 * 60_000));
    return gh(url, token, attempt + 1);
  }
  if (!res.ok) throw Object.assign(new Error(`GitHub ${res.status} for ${url}: ${(await res.text()).slice(0, 200)}`), { status: res.status });
  return res;
}

export type Get = (url: string) => Promise<any>;

/** Retries 5xx and network errors (no HTTP status); 4xx fail immediately. */
export function withRetry(get: Get, attempts = 3, baseMs = 500): Get {
  return async (url) => {
    for (let i = 1; ; i++) {
      try { return await get(url); } catch (e: any) {
        if (i >= attempts || (e?.status !== undefined && e.status < 500)) throw e;
        await sleep(baseMs * 2 ** (i - 1));
      }
    }
  };
}
const ghGet = (token: string): Get => async (url) => (await gh(url, token)).json();

/** Pages `base` (which already has per_page=100) with explicit &page=N until a short page. */
export async function pages<T>(base: string, get: Get, pick: (body: any) => T[], perPage = 100): Promise<{ items: T[]; total?: number }> {
  const items: T[] = [];
  let total: number | undefined;
  for (let page = 1; ; page++) {
    const body = await get(`${base}&page=${page}`);
    const got = pick(body);
    total ??= body.total_count;
    items.push(...got);
    if (got.length < perPage) return { items, total };
  }
}

const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d+Z$/, "Z");

/** Runs created in [from, to]; bisects any slice that hits the 1000-result listing cap. */
async function listRuns(repo: string, get: Get, from: number, to: number): Promise<any[]> {
  const q = new URLSearchParams({ created: `${iso(from)}..${iso(to)}`, per_page: "100" });
  const { items, total } = await pages(`https://api.github.com/repos/${repo}/actions/runs?${q}`, get, (b) => b.workflow_runs);
  if ((total ?? 0) <= 1000 || to - from < 2000) {
    if ((total ?? 0) > 1000) console.error(`warning: >1000 runs within ${iso(from)}..${iso(to)}; some omitted`);
    return items;
  }
  const mid = Math.floor((from + to) / 2000) * 1000;
  return [...(await listRuns(repo, get, from, mid)), ...(await listRuns(repo, get, mid + 1000, to))];
}

export async function mapLimit<T, R>(items: T[], limit: number, fn: (t: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) { const i = next++; out[i] = await fn(items[i]); }
  }));
  return out;
}

export async function fetchJobs(repo: string, days: number, token: string, concurrency = 4, get: Get = ghGet(token), retryBaseMs = 500): Promise<Job[]> {
  get = withRetry(get, 3, retryBaseMs);
  const end = Date.now(), start = end - days * 86_400_000, day = 86_400_000;
  const slices: [number, number][] = [];
  for (let s = start; s < end; s += day) slices.push([s, Math.min(s + day - 1000, end)]);
  const runs = new Map<number, any>();
  for (const r of (await mapLimit(slices, concurrency, ([a, b]) => listRuns(repo, get, a, b))).flat()) runs.set(r.id, r);
  console.error(`fetching jobs for ${runs.size} runs`);
  const perRun = await mapLimit([...runs.values()], concurrency, async (run) => {
    const { items } = await pages<Job>(`https://api.github.com/repos/${repo}/actions/runs/${run.id}/jobs?filter=all&per_page=100`, get, (b) => b.jobs);
    return items.map((j) => ({ ...j, event: run.event, run_name: run.name }));
  });
  return perRun.flat();
}

export async function run(argv: string[], env: Record<string, string | undefined> = process.env, stdin: () => Promise<string> = () => Bun.stdin.text()): Promise<string> {
  const { values } = parseArgs({
    args: argv,
    options: {
      repo: { type: "string", default: "waddle-social/waddle" }, days: { type: "string", default: "7" },
      json: { type: "boolean", default: false }, stdin: { type: "boolean", default: false }, top: { type: "string", default: "25" },
      "profile-vcpu": { type: "string", default: "8" }, rate: { type: "string", multiple: true },
    },
  });
  const days = Number(values.days), top = Number(values.top), profileVcpu = Number(values["profile-vcpu"]);
  if (![days, top, profileVcpu].every((x) => Number.isFinite(x) && x > 0)) throw new Error("--days, --top and --profile-vcpu must be positive numbers");
  const rates = parseRates(values.rate);

  let jobs: Job[], label: string;
  if (values.stdin) { jobs = parseJobs(await stdin()); label = "stdin (offline)"; }
  else {
    const token = env.GITHUB_TOKEN || env.GH_TOKEN;
    if (!token) throw new Error("Set GITHUB_TOKEN (or GH_TOKEN), or pass --stdin with saved jobs JSON on standard input.");
    jobs = await fetchJobs(values.repo!, days, token);
    label = `${values.repo}, last ${days} day(s)`;
  }
  const report = aggregate(toRecords(jobs, profileVcpu), { top, rates });
  return values.json ? JSON.stringify({ label, profileVcpu, ...report }, null, 2) + "\n" : format(report, { profileVcpu, label, rates });
}

if (import.meta.main) {
  run(process.argv.slice(2)).then((s) => process.stdout.write(s), (e) => { console.error(`error: ${e.message}`); process.exit(1); });
}
