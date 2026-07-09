import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import {
  requireArtifactManifestForCompleteGateZeroRecord,
  requireSharedGateZeroRelease,
  resolveTrustedEvidenceFile,
  validateArtifactManifestReference,
  type GateZeroArtifactEvidenceKind,
  type ValidatedGateZeroArtifactManifest,
} from "./support/gate-evidence";
import { parseJsonDocument } from "../scripts/switchable-baseline/json";
import { validateJourneyBaselineManifestReference } from "../scripts/switchable-baseline/journey-baseline";
import { verifyAttestedGateZeroPackage } from "../scripts/switchable-baseline/attested-package";
import {
  deriveGateReadiness,
  deriveJourneyEvidenceStatus,
  immutableGateEvidenceKinds,
  parseScenarioBinding,
  requireCompleteEvidenceReferencePolicy,
  requireExactScenarioReferenceBinding,
  requireImmutableGateKinds,
  requireImmutableJourneyKinds,
  requireUniqueCompletedKinds,
  scenarioTestId,
  type ProgramGateId,
  type ScenarioBinding,
} from "./support/switchable-program";

const repositoryRoot = resolve(import.meta.dir, "..");
const contractPath = resolve(repositoryRoot, "docs/product/critical-journeys.json");
const contract = parseJsonDocument(
  await Bun.file(contractPath).text(),
  "critical journey contract",
) as any;
const capabilityManifest = Bun.TOML.parse(
  await Bun.file(resolve(repositoryRoot, "server/capabilities.toml")).text()
) as { capability: Array<{ id: string; protocols: string[] }> };
const performanceProfilePath = resolve(repositoryRoot, contract.performanceProfile);
const performanceProfile = parseJsonDocument(
  await Bun.file(performanceProfilePath).text(),
  "performance profile",
) as any;

const expectedClients = ["desktop-web", "android-pwa", "ios", "macos"];
const expectedTopologies = ["local", "federated"];
const sorted = (values: string[]) => [...values].sort();

async function requireReadyGateZeroEvidence(
  status: string,
  completedKinds: Set<string>,
  manifests: ValidatedGateZeroArtifactManifest[],
): Promise<void> {
  if (status !== "ready") return;
  for (const kind of immutableGateEvidenceKinds["0"]) {
    if (!completedKinds.has(kind)) throw new Error(`ready Gate 0 is missing ${kind}`);
  }
  const manifestKinds = manifests.map(({ evidenceKind }) => evidenceKind).sort();
  if (
    JSON.stringify(manifestKinds)
    !== JSON.stringify(["capability-baseline", "telemetry-baseline"])
  ) {
    throw new Error("ready Gate 0 requires exactly one capability and telemetry manifest");
  }
  requireSharedGateZeroRelease(manifests);
  await verifyAttestedGateZeroPackage(repositoryRoot, manifests);
}

type ReferenceContext = {
  gateId: ProgramGateId;
  kind: string;
  status: "partial" | "complete";
  binding?: ScenarioBinding;
  artifactKind?: GateZeroArtifactEvidenceKind;
};

function expectExactKeys(
  value: Record<string, unknown>,
  keys: string[],
): void {
  expect(Object.keys(value).sort()).toEqual([...keys].sort());
}

async function validateManualSchemaReference(
  reference: Record<string, unknown>,
  context: ReferenceContext,
  root = repositoryRoot,
): Promise<void> {
  const scope = context.binding
    ? {
      type: "scenario",
      scenarioId: context.binding.scenarioId,
      client: context.binding.client,
      topology: context.binding.topology,
    }
    : { type: "gate", gate: Number(context.gateId) };
  const expectedSchema = `switchable-evidence/gate-${context.gateId}/${context.kind}/v1`;
  const expectedPath = context.binding
    ? `docs/evidence/gate-${context.gateId}/journeys/${context.binding.scenarioId}/${context.kind}.json`
    : `docs/evidence/gate-${context.gateId}/${context.kind}.json`;
  expectExactKeys(reference, ["type", "path", "sha256", "schema", ...(context.binding
    ? ["scenarioId", "client", "topology", "kind"]
    : [])]);
  expect(reference.path).toBe(expectedPath);
  expect(reference.schema).toBe(expectedSchema);
  expect(reference.sha256).toMatch(/^[0-9a-f]{64}$/);
  const path = resolveTrustedEvidenceFile(
    root,
    String(reference.path),
    "manual-schema reference.path",
    ".json",
  );
  expect(createHash("sha256").update(readFileSync(path)).digest("hex"))
    .toBe(reference.sha256);
  const report = parseJsonDocument(
    readFileSync(path, "utf8"),
    "typed manual evidence",
  ) as Record<string, unknown>;
  expectExactKeys(report, [
    "schemaVersion",
    "schema",
    "evidenceKind",
    "status",
    "scope",
    "release",
    "window",
    "capturedAt",
    "assertions",
  ]);
  expect(report.schemaVersion).toBe(1);
  expect(report.schema).toBe(expectedSchema);
  expect(report.evidenceKind).toBe(context.kind);
  expect(report.status).toBe("complete");
  expect(report.scope).toEqual(scope);
  expect(report.release).toEqual({
    serverCommit: expect.stringMatching(/^[0-9a-f]{40}$/),
    webCommit: expect.stringMatching(/^[0-9a-f]{40}$/),
    ...(context.binding
      ? { appCommit: expect.stringMatching(/^[0-9a-f]{40}$/) }
      : {}),
  });
  const evidenceWindow = report.window as Record<string, unknown>;
  expectExactKeys(evidenceWindow, ["start", "end"]);
  for (const value of [evidenceWindow.start, evidenceWindow.end, report.capturedAt]) {
    expect(value).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$/);
    expect(Number.isFinite(Date.parse(String(value)))).toBeTrue();
  }
  expect(Date.parse(String(evidenceWindow.end))).toBeGreaterThan(
    Date.parse(String(evidenceWindow.start)),
  );
  expect(Date.parse(String(report.capturedAt))).toBeGreaterThanOrEqual(
    Date.parse(String(evidenceWindow.end)),
  );
  expect(Array.isArray(report.assertions)).toBeTrue();
  expect((report.assertions as unknown[]).length).toBeGreaterThan(0);
  const assertionIds = new Set<string>();
  for (const [index, value] of (report.assertions as unknown[]).entries()) {
    expect(value).toBeObject();
    const assertion = value as Record<string, unknown>;
    expectExactKeys(assertion, ["id", "status", "observed"]);
    expect(assertion.id).toMatch(/^[a-z][a-z0-9-]+$/);
    expect(assertionIds.has(String(assertion.id))).toBeFalse();
    assertionIds.add(String(assertion.id));
    expect(assertion.status).toBe("pass");
    if (
      typeof assertion.observed !== "boolean"
      && (typeof assertion.observed !== "number" || !Number.isFinite(assertion.observed))
    ) {
      throw new Error(`typed manual evidence assertion ${index} has an invalid observed value`);
    }
  }
}

async function validateEvidenceReference(
  reference: Record<string, unknown>,
  context: ReferenceContext,
): Promise<ValidatedGateZeroArtifactManifest | undefined> {
  requireCompleteEvidenceReferencePolicy(context.gateId, {
    kind: context.kind,
    status: context.status,
    reference,
  }, context.binding);
  expect([
    "repo-test",
    "ci-run",
    "manual-report",
    "artifact-manifest",
    "journey-baseline-manifest",
    "manual-schema",
  ]).toContain(reference.type);
  if (reference.type === "repo-test") {
    if (context.status === "complete" && context.binding) {
      expectExactKeys(reference, [
        "type",
        "path",
        "testId",
        "scenarioId",
        "client",
        "topology",
        "kind",
      ]);
      requireExactScenarioReferenceBinding(reference, context.binding);
      expect(reference.testId).toBe(scenarioTestId(context.binding));
    }
    expect(reference.path).toMatch(
      /^(chat\/tests\/|server\/crates\/(?:[^/]+\/tests\/|waddle-server\/src\/server\/routes\/websocket\/tests\/)|apps\/apple\/.+\/Tests\/|tests\/)/
    );
    const path = resolve(repositoryRoot, String(reference.path));
    expect(existsSync(path)).toBeTrue();
    expect(reference.testId).toMatch(/^[A-Za-z][A-Za-z0-9_. :>-]+$/);
    const source = await Bun.file(path).text();
    const escapedId = String(reference.testId).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    if (String(reference.path).endsWith(".rs")) {
      expect(source).toMatch(new RegExp(`#\\[(?:tokio::)?test\\][\\s\\S]{0,240}(?:async\\s+)?fn\\s+${escapedId}\\s*\\(`));
    } else if (String(reference.path).endsWith(".ts")) {
      expect(source).toMatch(new RegExp(`(?:test|it)\\(\\s*["']${escapedId}["']`));
    } else if (String(reference.path).endsWith(".swift")) {
      const swiftTesting = new RegExp(`@Test[\\s\\S]{0,240}func\\s+${escapedId}\\s*\\(`).test(source);
      const xctest = /^test[A-Z0-9_]/.test(String(reference.testId))
        && new RegExp(`func\\s+${escapedId}\\s*\\(`).test(source);
      expect(swiftTesting || xctest).toBeTrue();
    } else if (String(reference.path).endsWith(".cue")) {
      expect(source).toMatch(new RegExp(`name:\\s*["']${escapedId}["']`));
    } else {
      throw new Error(`unsupported repo-test evidence path: ${reference.path}`);
    }
  } else if (reference.type === "ci-run") {
    expect(reference.url).toMatch(/^https:\/\/github\.com\/waddle-social\/waddle\/actions\/runs\/[0-9]+$/);
    expect(reference.commit).toMatch(/^[0-9a-f]{40}$/);
  } else if (reference.type === "manual-report") {
    const reportPath = resolveTrustedEvidenceFile(
      repositoryRoot,
      String(reference.path),
      "manual-report reference.path",
      ".md",
    );
    expect(reference.sha256).toMatch(/^[0-9a-f]{64}$/);
    const digest = createHash("sha256").update(readFileSync(reportPath)).digest("hex");
    expect(reference.sha256).toBe(digest);
  } else if (reference.type === "manual-schema") {
    await validateManualSchemaReference(reference, context);
  } else if (reference.type === "journey-baseline-manifest") {
    if (context.gateId !== "0" || context.kind !== "journey-baseline") {
      throw new Error("journey-baseline manifest is only valid for Gate 0 journey-baseline");
    }
    await validateJourneyBaselineManifestReference(repositoryRoot, reference);
  } else {
    if (!context.artifactKind) {
      throw new Error("artifact-manifest evidence is only valid for a typed Gate 0 artifact kind");
    }
    return validateArtifactManifestReference(
      repositoryRoot,
      reference,
      context.artifactKind,
    );
  }
  return undefined;
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
      requireImmutableJourneyKinds(journey.id, journey.evidence.requiredKinds);
      expect(new Set(journey.clients).size).toBe(journey.clients.length);
      expect(new Set(journey.topologies).size).toBe(journey.topologies.length);

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
    const journeyStatuses = new Map<string, "missing" | "partial" | "complete">();

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
        const binding = parseScenarioBinding(record.scenarioId, record.kind);
        await validateEvidenceReference(record.reference, {
          gateId: String(journey.gate) as ProgramGateId,
          kind: record.kind,
          status: record.status,
          binding,
        });
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
      }

      const derivedStatus = deriveJourneyEvidenceStatus(
        journeyScenarioIds,
        journey.evidence.requiredKinds,
        journey.evidence.records,
      );
      expect(journey.evidence.defaultStatus).toBe(derivedStatus);
      journeyStatuses.set(journey.id, derivedStatus);
    }

    for (const gateId of ["2", "3", "4"] as const) {
      const gateJourneyIds = contract.journeys
        .filter(({ gate }: { gate: number }) => String(gate) === gateId)
        .map(({ id }: { id: string }) => id);
      const derivedReadiness = gateJourneyIds.every(
        (journeyId: string) => journeyStatuses.get(journeyId) === "complete",
      ) ? "ready" : "not-ready";
      expect(contract.gateReadiness[gateId]).toBe(derivedReadiness);
    }
    const derivedJourneyReadiness = [...journeyStatuses.values()].every(
      (status) => status === "complete",
    ) ? "ready" : "not-ready";
    expect(contract.journeyStatus).toBe(derivedJourneyReadiness);
    expect(allScenarioIds.size).toBeGreaterThanOrEqual(100);
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
    const ledger = parseJsonDocument(
      await Bun.file(ledgerPath).text(),
      "gate evidence ledger",
    ) as any;
    expect(ledger.schemaVersion).toBe(1);
    expect(Object.keys(ledger.gates).sort()).toEqual(["0", "1", "2", "3", "4", "5"]);

    for (const [gateId, gate] of Object.entries(ledger.gates) as Array<[string, {
      status: string;
      requiredKinds: string[];
      records: Array<{
        kind: string;
        status: "partial" | "complete";
        reference: Record<string, unknown>;
      }>;
    }]>) {
      const typedGateId = gateId as ProgramGateId;
      const validatedGateZeroManifests: ValidatedGateZeroArtifactManifest[] = [];
      expect(["not-ready", "ready"]).toContain(gate.status);
      requireImmutableGateKinds(gateId, gate.requiredKinds);
      if (["2", "3", "4"].includes(gateId)) {
        expect(gate.records).toEqual([]);
        expect(gate.status).toBe(contract.gateReadiness[gateId]);
        continue;
      }
      requireUniqueCompletedKinds(gate.records, `gate ${gateId}`);
      const completedKinds = new Set<string>();
      for (const record of gate.records) {
        expect(gate.requiredKinds).toContain(record.kind);
        expect(["partial", "complete"]).toContain(record.status);
        const artifactKind = ["capability-baseline", "telemetry-baseline"].includes(record.kind)
          ? record.kind as GateZeroArtifactEvidenceKind
          : undefined;
        if (gateId === "0") {
          requireArtifactManifestForCompleteGateZeroRecord(record);
        }
        const validatedManifest = await validateEvidenceReference(record.reference, {
          gateId: typedGateId,
          kind: record.kind,
          status: record.status,
          artifactKind,
        });
        if (gateId === "0" && record.status === "complete" && validatedManifest) {
          validatedGateZeroManifests.push(validatedManifest);
        }
        if (record.status === "complete") completedKinds.add(record.kind);
      }
      if (gateId === "0") requireSharedGateZeroRelease(validatedGateZeroManifests);
      if (gateId === "0") {
        await requireReadyGateZeroEvidence(
          deriveGateReadiness(gate.requiredKinds, completedKinds),
          completedKinds,
          validatedGateZeroManifests,
        );
      }
      expect(gate.status).toBe(deriveGateReadiness(gate.requiredKinds, completedKinds));
    }
  });

  test("freezes every evidence scope and derives readiness only from validated completions", async () => {
    expect(() => requireImmutableGateKinds("0", ["journey-baseline"]))
      .toThrow("immutable program contract");
    expect(() => requireImmutableJourneyKinds("moderation", ["e2e"]))
      .toThrow("immutable program contract");
    await expect(requireReadyGateZeroEvidence(
      "ready",
      new Set(["journey-baseline"]),
      [],
    )).rejects.toThrow("missing capability-baseline");
    expect(deriveGateReadiness(["a", "b"], new Set(["a"]))).toBe("not-ready");
    expect(() => requireUniqueCompletedKinds([
      { kind: "security", status: "complete", reference: {} },
      { kind: "security", status: "complete", reference: {} },
    ], "gate 1")).toThrow("at most one complete record");
  });

  test("does not accept generic repository tests for operational or pilot outcomes", () => {
    const genericReference = {
      type: "repo-test",
      path: "tests/critical-journeys.test.ts",
      testId: "covers every release dimension and named critical journey",
    };
    for (const [gateId, kind] of [
      ["1", "availability-slo"],
      ["5", "hosted-pilot"],
    ] as const) {
      expect(() => requireCompleteEvidenceReferencePolicy(gateId, {
        kind,
        status: "complete",
        reference: genericReference,
      })).toThrow("verified kind-specific operational or pilot attestation");
    }

    const gateFourBinding = parseScenarioBinding(
      "moderation/report-and-block/desktop-web/local",
      "metric",
    );
    expect(() => requireCompleteEvidenceReferencePolicy("4", {
      kind: "metric",
      status: "complete",
      reference: { ...genericReference, ...gateFourBinding },
    }, gateFourBinding)).toThrow("manual/live evidence attestation");

    const gateTwoBinding = parseScenarioBinding(
      "federation-interoperability/remote-dm/desktop-web/federated",
      "interop",
    );
    expect(() => requireCompleteEvidenceReferencePolicy("2", {
      kind: "interop",
      status: "complete",
      reference: genericReference,
    }, gateTwoBinding)).toThrow("exact record scope");
    expect(() => requireCompleteEvidenceReferencePolicy("2", {
      kind: "interop",
      status: "complete",
      reference: {
        ...genericReference,
        ...gateTwoBinding,
        testId: scenarioTestId(gateTwoBinding),
      },
    }, gateTwoBinding)).toThrow("passing-run or manual/live evidence attestation");
  });

  test("keeps hashed, bounded manual-schema evidence as partial context only", async () => {
    const root = await mkdtemp(join(tmpdir(), "waddle-program-evidence-"));
    const path = "docs/evidence/gate-1/availability-slo.json";
    const absolutePath = resolve(root, path);
    const schema = "switchable-evidence/gate-1/availability-slo/v1";
    const document = {
      schemaVersion: 1,
      schema,
      evidenceKind: "availability-slo",
      status: "complete",
      scope: { type: "gate", gate: 1 },
      release: {
        serverCommit: "0123456789abcdef0123456789abcdef01234567",
        webCommit: "1111111111111111111111111111111111111111",
      },
      window: {
        start: "2026-07-10T09:00:00Z",
        end: "2026-07-10T10:00:00Z",
      },
      capturedAt: "2026-07-10T10:05:00Z",
      assertions: [{ id: "availability-objective", status: "pass", observed: true }],
    };
    const contents = JSON.stringify(document, null, 2) + "\n";
    await mkdir(dirname(absolutePath), { recursive: true });
    await Bun.write(absolutePath, contents);
    const reference = {
      type: "manual-schema",
      path,
      schema,
      sha256: createHash("sha256").update(contents).digest("hex"),
    };
    const context: ReferenceContext = {
      gateId: "1",
      kind: "availability-slo",
      status: "partial",
    };
    try {
      await expect(validateManualSchemaReference(reference, context, root))
        .resolves.toBeUndefined();
      expect(() => requireCompleteEvidenceReferencePolicy("1", {
        kind: context.kind,
        status: "complete",
        reference,
      })).toThrow("verified kind-specific operational or pilot attestation");

      const unsafe = structuredClone(document);
      unsafe.assertions[0].observed = "alice@example.test" as never;
      const unsafeContents = JSON.stringify(unsafe, null, 2) + "\n";
      await Bun.write(absolutePath, unsafeContents);
      await expect(validateManualSchemaReference({
        ...reference,
        sha256: createHash("sha256").update(unsafeContents).digest("hex"),
      }, context, root)).rejects.toThrow("invalid observed value");
    } finally {
      await rm(root, { recursive: true, force: true });
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
