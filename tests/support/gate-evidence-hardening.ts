import { afterEach } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { ReplicaProvenance } from "../../scripts/switchable-baseline/replica-provenance";

const roots: string[] = [];

export const serverCommit = "0123456789abcdef0123456789abcdef01234567";
export const webCommit = "1111111111111111111111111111111111111111";
export const workflowCommit = "2222222222222222222222222222222222222222";
export const provenanceDigest = (value: string) =>
  createHash("sha256").update(value).digest("hex");
export const window = {
  start: "2026-07-10T09:00:00Z",
  end: "2026-07-10T10:00:00Z",
};
export const scope = {
  job: "waddle-server",
  environment: "production",
  cluster: "waddle-cloud",
  namespace: "waddle",
  expectedReplicas: 2,
  identityMetric: "waddle_build_info",
  targetSignalId: "server-deployment-identity-targets",
  identityLookbackSeconds: 3600,
};
export const replicaProvenance: ReplicaProvenance = {
  schemaVersion: 1,
  kind: "kubernetes-deployment",
  deployment: {
    apiVersion: "apps/v1",
    name: "waddle-server",
    namespace: "waddle",
    uidSha256: provenanceDigest("apps/v1/waddle/waddle-server/uid"),
    generation: 42,
    observedGeneration: 42,
    specReplicas: 2,
    configSha256: provenanceDigest("waddle-server deployment generation 42"),
  },
};

afterEach(async () => {
  await Promise.all(roots.splice(0).map((path) =>
    rm(path, { recursive: true, force: true })
  ));
});

export async function fixtureRoot(): Promise<string> {
  const path = await mkdtemp(join(tmpdir(), "waddle-evidence-hardening-"));
  roots.push(path);
  await mkdir(resolve(path, "docs/evidence"), { recursive: true });
  return path;
}
