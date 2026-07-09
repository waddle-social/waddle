import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

const repositoryRoot = resolve(import.meta.dir, "..");
const contractPath = resolve(repositoryRoot, "docs/product/critical-journeys.json");
const contract = await Bun.file(contractPath).json();
const capabilityManifest = Bun.TOML.parse(
  await Bun.file(resolve(repositoryRoot, "server/capabilities.toml")).text()
) as { capability: Array<{ id: string; protocols: string[] }> };
const performanceProfilePath = resolve(repositoryRoot, contract.performanceProfile);
const performanceProfile = await Bun.file(performanceProfilePath).json();

const expectedClients = ["desktop-web", "android-pwa", "ios", "macos"];
const expectedTopologies = ["local", "federated"];

const sorted = (values: string[]) => [...values].sort();

async function validateEvidenceReference(reference: Record<string, string>) {
  expect(["repo-test", "ci-run", "manual-report"]).toContain(reference.type);
  if (reference.type === "repo-test") {
    expect(reference.path).toMatch(
      /^(chat\/tests\/|server\/crates\/(?:[^/]+\/tests\/|waddle-server\/src\/server\/routes\/websocket\/tests\/)|apps\/apple\/.+\/Tests\/|tests\/)/
    );
    const path = resolve(repositoryRoot, reference.path);
    expect(existsSync(path)).toBeTrue();
    expect(reference.testId).toMatch(/^[A-Za-z][A-Za-z0-9_. :>-]+$/);
    const source = await Bun.file(path).text();
    const escapedId = reference.testId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    if (reference.path.endsWith(".rs")) {
      expect(source).toMatch(new RegExp(`#\\[(?:tokio::)?test\\][\\s\\S]{0,240}(?:async\\s+)?fn\\s+${escapedId}\\s*\\(`));
    } else if (reference.path.endsWith(".ts")) {
      expect(source).toMatch(new RegExp(`(?:test|it)\\(\\s*["']${escapedId}["']`));
    } else if (reference.path.endsWith(".swift")) {
      const swiftTesting = new RegExp(`@Test[\\s\\S]{0,240}func\\s+${escapedId}\\s*\\(`).test(source);
      const xctest = /^test[A-Z0-9_]/.test(reference.testId)
        && new RegExp(`func\\s+${escapedId}\\s*\\(`).test(source);
      expect(swiftTesting || xctest).toBeTrue();
    } else if (reference.path.endsWith(".cue")) {
      expect(source).toMatch(new RegExp(`name:\\s*["']${escapedId}["']`));
    } else {
      throw new Error(`unsupported repo-test evidence path: ${reference.path}`);
    }
  } else if (reference.type === "ci-run") {
    expect(reference.url).toMatch(/^https:\/\/github\.com\/waddle-social\/waddle\/actions\/runs\/[0-9]+$/);
    expect(reference.commit).toMatch(/^[0-9a-f]{40}$/);
  } else {
    expect(reference.path).toMatch(/^docs\/evidence\/.+\.md$/);
    const reportPath = resolve(repositoryRoot, reference.path);
    expect(existsSync(reportPath)).toBeTrue();
    expect(reference.sha256).toMatch(/^[0-9a-f]{64}$/);
    const digest = createHash("sha256").update(readFileSync(reportPath)).digest("hex");
    expect(reference.sha256).toBe(digest);
  }
}

describe("switchable-alternative critical journey contract", () => {
  test("uses the supported schema and release dimensions", async () => {
    expect(contract.schemaVersion).toBe(1);
    expect(contract.milestone).toBe("switchable-alternative");
    expect(["not-ready", "ready"]).toContain(contract.journeyStatus);
    expect(Object.keys(contract.gateReadiness).sort()).toEqual(["2", "3", "4"]);
    for (const status of Object.values(contract.gateReadiness)) {
      expect(["not-ready", "ready"]).toContain(status);
    }
    expect(sorted(contract.dimensions.clients)).toEqual(sorted(expectedClients));
    expect(sorted(contract.dimensions.topologies)).toEqual(sorted(expectedTopologies));
    expect(contract.releasePolicy).toBeString();
    expect(contract.releasePolicy.length).toBeGreaterThan(0);
    expect(contract.performanceProfile).toBe("docs/product/performance-profile.json");
    expect(performanceProfile.schemaVersion).toBe(1);
    expect(performanceProfile.dataset.members).toBe(500);
    expect(Object.keys(performanceProfile.clients).sort()).toEqual(sorted(expectedClients));
  });

  test("defines unique, complete, release-blocking scenarios", () => {
    const journeyIds = contract.journeys.map((journey: { id: string }) => journey.id);
    expect(new Set(journeyIds).size).toBe(journeyIds.length);

    const scenarioIds = new Set<string>();
    for (const journey of contract.journeys) {
      expect(journey.id).toMatch(/^[a-z][a-z0-9-]+$/);
      expect(journey.title).toBeString();
      expect(journey.owner).toMatch(/^[a-z][a-z0-9-]+$/);
      expect(Object.keys(contract.gateReadiness)).toContain(String(journey.gate));
      expect(journey.clients.length).toBeGreaterThan(0);
      for (const client of journey.clients) {
        expect(expectedClients).toContain(client);
      }
      expect(journey.topologies.length).toBeGreaterThan(0);
      for (const topology of journey.topologies) {
        expect(expectedTopologies).toContain(topology);
      }
      expect(journey.requirements.length).toBeGreaterThan(0);
      expect(["missing", "partial", "complete"]).toContain(journey.evidence.defaultStatus);
      expect(journey.evidence.requiredKinds.length).toBeGreaterThan(0);
      expect(new Set(journey.evidence.requiredKinds).size).toBe(journey.evidence.requiredKinds.length);

      const requirementIds = new Set<string>();
      for (const requirement of journey.requirements) {
        expect(requirement.id).toMatch(/^[a-z][a-z0-9-]+$/);
        expect(requirementIds.has(requirement.id)).toBeFalse();
        requirementIds.add(requirement.id);
        for (const field of ["given", "when", "then"] as const) {
          expect(requirement[field]).toBeString();
          expect(requirement[field].trim().length).toBeGreaterThan(0);
        }

        for (const client of journey.clients) {
          for (const topology of journey.topologies) {
            const scenarioId = `${journey.id}/${requirement.id}/${client}/${topology}`;
            expect(scenarioIds.has(scenarioId)).toBeFalse();
            scenarioIds.add(scenarioId);
          }
        }
      }
    }

    expect(scenarioIds.size).toBeGreaterThanOrEqual(100);
  });

  test("keeps scenario evidence explicit and blocks an unevidenced release", async () => {
    const allScenarioIds = new Set<string>();
    const completeEvidence = new Map<string, Set<string>>();

    for (const journey of contract.journeys) {
      const journeyScenarioIds = new Set<string>();
      for (const requirement of journey.requirements) {
        for (const client of journey.clients) {
          for (const topology of journey.topologies) {
            const scenarioId = `${journey.id}/${requirement.id}/${client}/${topology}`;
            allScenarioIds.add(scenarioId);
            journeyScenarioIds.add(scenarioId);
          }
        }
      }

      const evidenceKeys = new Set<string>();
      for (const record of journey.evidence.records) {
        expect(journeyScenarioIds).toContain(record.scenarioId);
        expect(["partial", "complete"]).toContain(record.status);
        expect(journey.evidence.requiredKinds).toContain(record.kind);
        await validateEvidenceReference(record.reference);
        if (record.kind === "performance") {
          expect(record.environment.profileId).toBe("switchable-500-v1");
          expect(expectedClients).toContain(record.environment.client);
          const expectedEnvironment = performanceProfile.clients[record.environment.client];
          expect(expectedEnvironment).toBeDefined();
          expect(record.environment.hardware).toBe(expectedEnvironment.hardware);
          expect(record.environment.ramGiB).toBe(expectedEnvironment.ramGiB);
          expect(record.environment.network).toEqual(performanceProfile.network);
          expect(record.environment.osVersion).toMatch(/^[A-Za-z0-9][A-Za-z0-9_. -]+$/);
          expect(record.environment.appCommit).toMatch(/^[0-9a-f]{40}$/);
          if (["desktop-web", "android-pwa"].includes(record.environment.client)) {
            expect(record.environment.browserVersion).toMatch(/^[A-Za-z]+\/[0-9]+(?:\.[0-9]+){1,3}$/);
          }
        }
        const key = `${record.scenarioId}/${record.kind}`;
        expect(evidenceKeys.has(key)).toBeFalse();
        evidenceKeys.add(key);
        if (record.status === "complete") {
          const kinds = completeEvidence.get(record.scenarioId) ?? new Set<string>();
          kinds.add(record.kind);
          completeEvidence.set(record.scenarioId, kinds);
        }
      }

      if (journey.evidence.defaultStatus === "complete") {
        expect(journey.evidence.records.length).toBe(journeyScenarioIds.size * journey.evidence.requiredKinds.length);
      }
    }

    if (contract.journeyStatus === "ready") {
      for (const status of Object.values(contract.gateReadiness)) {
        expect(status).toBe("ready");
      }
    }

    for (const [gate, status] of Object.entries(contract.gateReadiness)) {
      if (status !== "ready") continue;
      for (const scenarioId of allScenarioIds) {
        const journeyId = scenarioId.split("/", 1)[0];
        const journey = contract.journeys.find((entry: { id: string }) => entry.id === journeyId);
        expect(journey).toBeDefined();
        if (String(journey.gate) !== gate) continue;
        const kinds = completeEvidence.get(scenarioId) ?? new Set<string>();
        for (const requiredKind of journey.evidence.requiredKinds) {
          expect(kinds).toContain(requiredKind);
        }
      }
    }
  });

  test("references only vendored XEP specifications", () => {
    const protocolClaims = [
      ...contract.journeys.map((journey: { id: string; protocols: string[] }) => journey),
      ...capabilityManifest.capability
    ];
    for (const claim of protocolClaims) {
      expect(new Set(claim.protocols).size).toBe(claim.protocols.length);
      for (const protocol of claim.protocols) {
        expect(protocol).toMatch(/^XEP-[0-9]{4}$/);
        const number = protocol.slice("XEP-".length);
        expect(existsSync(resolve(repositoryRoot, `xeps/xep-${number}.xml`))).toBeTrue();
      }
    }
  });

  test("requires typed durable evidence for trust and pilot gates", async () => {
    const ledgerPath = resolve(repositoryRoot, "docs/product/gate-evidence.json");
    const ledger = await Bun.file(ledgerPath).json();
    expect(ledger.schemaVersion).toBe(1);
    expect(Object.keys(ledger.gates).sort()).toEqual(["0", "1", "5"]);

    for (const gate of Object.values(ledger.gates) as Array<{
      status: string;
      requiredKinds: string[];
      records: Array<{ kind: string; status: string; reference: Record<string, string> }>;
    }>) {
      expect(["not-ready", "ready"]).toContain(gate.status);
      expect(gate.requiredKinds.length).toBeGreaterThan(0);
      expect(new Set(gate.requiredKinds).size).toBe(gate.requiredKinds.length);
      const completedKinds = new Set<string>();
      for (const record of gate.records) {
        expect(gate.requiredKinds).toContain(record.kind);
        expect(["partial", "complete"]).toContain(record.status);
        await validateEvidenceReference(record.reference);
        if (record.status === "complete") completedKinds.add(record.kind);
      }
      if (gate.status === "ready") {
        for (const kind of gate.requiredKinds) expect(completedKinds).toContain(kind);
      }
    }
  });

  test("covers every release dimension and named critical journey", () => {
    const clients = new Set<string>();
    const topologies = new Set<string>();
    const journeyIds = new Set<string>();
    for (const journey of contract.journeys) {
      journeyIds.add(journey.id);
      journey.clients.forEach((client: string) => clients.add(client));
      journey.topologies.forEach((topology: string) => topologies.add(topology));
    }

    expect(sorted([...clients])).toEqual(sorted(expectedClients));
    expect(sorted([...topologies])).toEqual(sorted(expectedTopologies));
    expect(journeyIds).toEqual(
      new Set([
        "authenticate",
        "invite-and-join",
        "room-messaging",
        "direct-messaging",
        "history",
        "unread-state",
        "notifications",
        "search",
        "file-sharing",
        "threads-and-replies",
        "reactions",
        "moderation",
        "member-lifecycle-and-administration",
        "admin-operational-health",
        "calls",
        "reconnect",
        "multi-device",
        "accessibility",
        "keyboard-navigation",
        "performance-budgets",
        "pwa-lifecycle",
        "federation-interoperability",
        "federation-failure-isolation"
      ])
    );
  });
});
