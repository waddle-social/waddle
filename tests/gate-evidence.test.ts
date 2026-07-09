import { describe, expect, test } from "bun:test";
import { validatePrometheusArtifact } from "../scripts/switchable-baseline/gate-evidence/prometheus";
import { readRepositorySourceAtCommit } from "../scripts/switchable-baseline/gate-evidence/filesystem";
import { FARO_WEB_SOURCE_PATHS } from "../scripts/switchable-baseline/source-contract";
import {
  capabilityRoles,
  completeFixture,
  otherServerCommit,
  otherWebCommit,
  release,
  repositoryRoot,
  rewriteArtifact,
  serialize,
  serverCommit,
  sha256,
  telemetryRoles,
  validate,
  webCommit,
  window,
  writeFixtureFile,
  writeFixtureManifest,
} from "./support/gate-evidence-fixtures";

describe("Gate 0 artifact evidence", () => {
  test("accepts complete role-specific capability and aggregate telemetry evidence", async () => {
    const capability = await completeFixture("capability-baseline");
    const capabilityManifest = await validate(capability);
    expect(capabilityManifest.artifacts.map(({ role }) => role).sort()).toEqual(
      [...capabilityRoles].sort(),
    );

    const telemetry = await completeFixture("telemetry-baseline");
    const telemetryManifest = await validate(telemetry);
    expect(telemetryManifest.artifacts.map(({ role }) => role).sort()).toEqual(
      [...telemetryRoles].sort(),
    );
    expect(telemetryManifest.release).toEqual(release);
    expect(telemetryManifest.catalogSha256).toMatch(/^[0-9a-f]{64}$/);
    expect(telemetryManifest.deploymentScope.environment).toBe("production");
  });

  test("rejects arbitrary JSON, CSV, role confusion, and raw Faro session metadata", async () => {
    const arbitrary = await completeFixture("telemetry-baseline");
    await rewriteArtifact(arbitrary, "faro-browser-auth-bootstrap", (value) => {
      for (const key of Object.keys(value)) delete value[key];
      Object.assign(value, { role: "faro-browser-auth-bootstrap", sample: 1 });
    });
    await expect(validate(arbitrary)).rejects.toThrow("must contain exactly");

    const roleConfusion = await completeFixture("telemetry-baseline");
    await rewriteArtifact(roleConfusion, "faro-browser-auth-bootstrap", (value) => {
      value.role = "faro-browser-session-lifecycle";
    });
    await expect(validate(roleConfusion)).rejects.toThrow("role must match");

    const rawMetadata = await completeFixture("telemetry-baseline");
    await rewriteArtifact(rawMetadata, "faro-browser-auth-bootstrap", (value) => {
      value.meta = {
        session: { id: "secret-session" },
        user: { id: "alice@example.test" },
      };
    });
    await expect(validate(rawMetadata)).rejects.toThrow("must contain exactly");

    const csv = await completeFixture("telemetry-baseline");
    csv.artifacts[1].path = "docs/evidence/gate-0/faro.csv";
    csv.artifacts[1].sha256 = await writeFixtureFile(csv.root, csv.artifacts[1].path, "unsafe,raw\n");
    csv.reference = await writeFixtureManifest(csv);
    await expect(validate(csv)).rejects.toThrow("normalized JSON artifact");
  });

  test("rejects duplicate JSON keys even when the last value is valid", async () => {
    const fixture = await completeFixture("telemetry-baseline");
    const role = "faro-browser-auth-bootstrap";
    const artifact = fixture.artifacts.find((entry) => entry.role === role);
    if (!artifact) throw new Error("missing duplicate-key fixture artifact");
    const valid = serialize(fixture.contents.get(role));
    const duplicateKey = "alice@example.com-access-token";
    const unsafe = valid.replace(
      '  "series": [',
      `  "${duplicateKey}": null,\n  "${duplicateKey}": null,\n  "series": [`,
    );
    artifact.sha256 = await writeFixtureFile(fixture.root, artifact.path, unsafe);
    fixture.reference = await writeFixtureManifest(fixture);

    const error = await validate(fixture).then(
      () => undefined,
      (failure: unknown) => failure,
    );
    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toContain("duplicate object key");
    expect((error as Error).message).not.toContain(duplicateKey);
  });

  test("rejects an arbitrary claimed commit when source bytes cannot be read there", async () => {
    for (const kind of ["capability-baseline", "telemetry-baseline"] as const) {
      const fixture = await completeFixture(kind, undefined, {
        serverCommit: otherServerCommit,
        webCommit: otherWebCommit,
      });
      await expect(validate(fixture, release)).rejects.toThrow(
        "could not read " + (kind === "telemetry-baseline"
          ? "docs/observability/switchable-baseline-signals.json"
          : "server/capabilities.toml") + " at the asserted evidence commit",
      );
    }
    await expect(
      readRepositorySourceAtCommit(
        repositoryRoot,
        otherServerCommit,
        "docs/observability/switchable-baseline-signals.json",
      ),
    ).rejects.toThrow("at the asserted evidence commit");

    const telemetry = await completeFixture("telemetry-baseline");
    await expect(validate(telemetry, {
      serverCommit,
      webCommit: otherWebCommit,
    })).rejects.toThrow(
      FARO_WEB_SOURCE_PATHS[0] + " at the asserted evidence commit",
    );
  });

  test("binds every telemetry artifact to one release tuple, window, catalog, and deployment scope", async () => {
    const mixedCommit = await completeFixture("telemetry-baseline");
    mixedCommit.artifacts[1].release = {
      serverCommit,
      webCommit: otherWebCommit,
    };
    mixedCommit.reference = await writeFixtureManifest(mixedCommit);
    await expect(validate(mixedCommit)).rejects.toThrow("release must match");

    const mixedWindow = await completeFixture("telemetry-baseline");
    await rewriteArtifact(mixedWindow, "faro-browser-message-ack-latency", (value) => {
      value.window = {
        start: "2026-07-10T10:00:00Z",
        end: "2026-07-10T11:00:00Z",
      };
    });
    await expect(validate(mixedWindow)).rejects.toThrow("window must match");

    const mixedScope = await completeFixture("telemetry-baseline");
    await rewriteArtifact(mixedScope, "faro-browser-session-lifecycle", (value) => {
      (value.scope as Record<string, unknown>).deploymentEnvironment = "staging";
    });
    await expect(validate(mixedScope)).rejects.toThrow("match Prometheus scope");

    const unknownScope = await completeFixture("telemetry-baseline");
    await rewriteArtifact(unknownScope, "prometheus-baseline", (value) => {
      (value.deploymentScope as Record<string, unknown>).namespace = "unknown";
    });
    await expect(validate(unknownScope)).rejects.toThrow(
      "bounded lowercase deployment label",
    );

    const wrongCatalog = await completeFixture("telemetry-baseline");
    await rewriteArtifact(wrongCatalog, "prometheus-baseline", (value) => {
      (value.catalog as Record<string, unknown>).sha256 = "b".repeat(64);
    });
    await expect(validate(wrongCatalog)).rejects.toThrow("SHA-256 does not match the expected bytes");
  });

  test("requires exact Faro dimensions, aggregate counts, and catalog query", async () => {
    const unsafeDimension = await completeFixture("telemetry-baseline");
    await rewriteArtifact(unsafeDimension, "faro-browser-message-ack-latency", (value) => {
      (value.dimensions as Record<string, unknown>).session = ["secret"];
    });
    await expect(validate(unsafeDimension)).rejects.toThrow("must contain exactly");

    const unexpectedValue = await completeFixture("telemetry-baseline");
    await rewriteArtifact(unexpectedValue, "faro-browser-message-ack-latency", (value) => {
      const series = value.series as Array<Record<string, unknown>>;
      (series[0].attributes as Record<string, unknown>).kind = "channel-secret";
    });
    await expect(validate(unexpectedValue)).rejects.toThrow("outside the catalog closed set");

    const rawRowCount = await completeFixture("telemetry-baseline");
    await rewriteArtifact(rawRowCount, "faro-browser-reconnect-duration", (value) => {
      (value.source as Record<string, unknown>).rowCount = 4000;
    });
    await expect(validate(rawRowCount)).rejects.toThrow("rowCount must equal");

    const changedQuery = await completeFixture("telemetry-baseline");
    await rewriteArtifact(changedQuery, "faro-browser-auth-bootstrap", (value) => {
      (value.source as Record<string, unknown>).query = "select raw sessions";
    });
    await expect(validate(changedQuery)).rejects.toThrow(
      "query must match the exact catalog release/environment/window plan",
    );

    const emptyJourney = await completeFixture("telemetry-baseline");
    await rewriteArtifact(emptyJourney, "faro-browser-session-lifecycle", (value) => {
      for (const row of value.series as Array<Record<string, unknown>>) row.count = 0;
    });
    await expect(validate(emptyJourney)).rejects.toThrow("required activity");

    const missingDm = await completeFixture("telemetry-baseline");
    await rewriteArtifact(missingDm, "faro-browser-message-ack-latency", (value) => {
      const rows = value.series as Array<Record<string, unknown>>;
      const dm = rows.find((row) =>
        (row.attributes as Record<string, unknown>).kind === "dm"
      );
      if (dm) {
        dm.count = 0;
        dm.latencyMs = { p50: null, p95: null };
      }
    });
    await expect(validate(missingDm)).rejects.toThrow('required activity {"kind":"dm"}');

    const wrongReleaseFilter = await completeFixture("telemetry-baseline");
    await rewriteArtifact(wrongReleaseFilter, "faro-browser-session-lifecycle", (value) => {
      const source = value.source as Record<string, unknown>;
      (source.query as Record<string, unknown>).release = otherWebCommit;
    });
    await expect(validate(wrongReleaseFilter)).rejects.toThrow(
      "exact catalog release/environment/window plan",
    );
  });

  test("enforces loss, delivery-drop, replica-continuity, and complete sample-grid assertions", async () => {
    const loss = await completeFixture("telemetry-baseline");
    await rewriteArtifact(loss, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const signal = signals.find(({ id }) => id === "loss-corruption-safety");
      const series = signal?.series as Array<Record<string, unknown>>;
      (series[0].samples as Array<Record<string, unknown>>)[4].value = 1;
    });
    await expect(validate(loss)).rejects.toThrow("must be zero");

    const drop = await completeFixture("telemetry-baseline");
    await rewriteArtifact(drop, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const signal = signals.find(({ id }) => id === "live-delivery-channel-outcomes");
      const series = signal?.series as Array<Record<string, unknown>>;
      const dropped = series.find((entry) =>
        (entry.attributes as Record<string, unknown>).outcome === "dropped_full"
      );
      (dropped?.samples as Array<Record<string, unknown>>)[4].value = 1;
    });
    await expect(validate(drop)).rejects.toThrow("zero dropped_full");

    const replicas = await completeFixture("telemetry-baseline");
    await rewriteArtifact(replicas, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const signal = signals.find(({ id }) => id === "server-deployment-identity-targets");
      const series = signal?.series as Array<Record<string, unknown>>;
      (series[0].samples as Array<Record<string, unknown>>)[4].value = 1;
    });
    await expect(validate(replicas)).rejects.toThrow("expected replica count");

    const restartedProcess = await completeFixture("telemetry-baseline");
    await rewriteArtifact(restartedProcess, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const continuity = signals.find(({ id }) => id === "server-process-start-continuity");
      const series = continuity?.series as Array<Record<string, unknown>>;
      (series[0].samples as Array<Record<string, unknown>>)[4].value = 2;
    });
    await expect(validate(restartedProcess)).rejects.toThrow(
      "must remain constant across the complete collection grid",
    );

    const missingSample = await completeFixture("telemetry-baseline");
    await rewriteArtifact(missingSample, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const series = signals[0].series as Array<Record<string, unknown>>;
      (series[0].samples as unknown[]).splice(4, 1);
    });
    await expect(validate(missingSample)).rejects.toThrow("complete fixed timestamp grid");

    const missingIdentityHistory = await completeFixture("telemetry-baseline");
    await rewriteArtifact(missingIdentityHistory, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const identity = signals.find(({ id }) => id === "server-deployment-identity-targets");
      const series = identity?.series as Array<Record<string, unknown>>;
      (series[0].samples as unknown[]).shift();
    });
    await expect(validate(missingIdentityHistory)).rejects.toThrow("complete fixed timestamp grid");

    const staleRoomSample = await completeFixture("telemetry-baseline");
    await rewriteArtifact(staleRoomSample, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const freshness = signals.find(({ id }) => id === "room-registry-sample-freshness");
      const series = freshness?.series as Array<Record<string, unknown>>;
      (series[0].samples as Array<Record<string, unknown>>)[4].value = 61;
    });
    await expect(validate(staleRoomSample)).rejects.toThrow("above the catalog maximum");

    const sparseGrid = await completeFixture("telemetry-baseline");
    await rewriteArtifact(sparseGrid, "prometheus-baseline", (value) => {
      (value.collectionWindow as Record<string, unknown>).stepSeconds = 10_800;
    });
    await expect(validate(sparseGrid)).rejects.toThrow("stepSeconds must be exactly 60");

    const misalignedGrid = await completeFixture("telemetry-baseline");
    const prometheusEntry = misalignedGrid.artifacts.find(
      ({ role }) => role === "prometheus-baseline",
    );
    const prometheusValue = structuredClone(
      misalignedGrid.contents.get("prometheus-baseline"),
    );
    if (!prometheusEntry || !prometheusValue) {
      throw new Error("missing Prometheus grid fixture");
    }
    const shiftedWindow = {
      start: "2026-07-10T09:00:30Z",
      end: "2026-07-10T10:00:30Z",
    };
    Object.assign(
      prometheusValue.collectionWindow as Record<string, unknown>,
      shiftedWindow,
    );
    expect(() =>
      validatePrometheusArtifact(
        misalignedGrid.root,
        prometheusValue,
        {
          ...prometheusEntry,
          role: "prometheus-baseline",
          window: shiftedWindow,
        },
      )
    ).toThrow("align exactly to the fixed timestamp grid");

    const noDmArchive = await completeFixture("telemetry-baseline");
    await rewriteArtifact(noDmArchive, "prometheus-baseline", (value) => {
      const signals = (value.automatedPrometheus as Record<string, unknown>)
        .signals as Array<Record<string, unknown>>;
      const archive = signals.find(({ id }) => id === "message-archive-attempts");
      const rows = archive?.series as Array<Record<string, unknown>>;
      const dmCommitted = rows.find((row) => {
        const attributes = row.attributes as Record<string, unknown>;
        return attributes.kind === "dm" && attributes.outcome === "committed";
      });
      const samples = dmCommitted?.samples as Array<Record<string, unknown>>;
      if (samples?.length && dmCommitted) {
        samples[samples.length - 1].value = 0;
        dmCommitted.canonicalEndSample = { ...samples[samples.length - 1] };
      }
    });
    await expect(validate(noDmArchive)).rejects.toThrow("required activity");
  });

  test("requires live disco and reconciliation to match the checked-in capability manifest", async () => {
    const missingFeature = await completeFixture("capability-baseline");
    await rewriteArtifact(missingFeature, "capability-reconciliation", (value) => {
      const checks = value.checks as Array<Record<string, unknown>>;
      const targets = checks[0].targets as Array<Record<string, unknown>>;
      (targets[0].observedFeatures as string[]).splice(0, 1);
    });
    await expect(validate(missingFeature)).rejects.toThrow(
      "observedFeatures must equal the sorted closed set",
    );

    const arbitrary = await completeFixture("capability-baseline");
    await rewriteArtifact(arbitrary, "live-disco-export", (value) => {
      for (const key of Object.keys(value)) delete value[key];
      Object.assign(value, { role: "live-disco-export", sample: 1 });
    });
    await expect(validate(arbitrary)).rejects.toThrow("must contain exactly");

    const missingRole = await completeFixture("capability-baseline");
    missingRole.artifacts.splice(1, 1);
    missingRole.reference = await writeFixtureManifest(missingRole);
    await expect(validate(missingRole)).rejects.toThrow("requires exactly these artifact roles");

    const unexpectedOfficial = await completeFixture("capability-baseline");
    await rewriteArtifact(unexpectedOfficial, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      (entities[0].features as string[]).push("http://jabber.org/protocol/muc");
      (entities[0].features as string[]).sort();
    });
    await expect(validate(unexpectedOfficial)).rejects.toThrow(
      "synthetic union feature owned by muc-service",
    );

    const runtimeCustom = await completeFixture("capability-baseline");
    await rewriteArtifact(runtimeCustom, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      for (const target of ["server", "representative-muc-room"]) {
        const entity = entities.find((entry) => entry.target === target);
        if (!entity) throw new Error("missing runtime-dependent target fixture");
        (entity.features as string[]).push("urn:waddle:extension:installed:0");
        (entity.features as string[]).sort();
      }
    });
    await expect(validate(runtimeCustom)).rejects.toThrow(
      "outside the exact checked-in target registry",
    );

    const runtimeOfficial = await completeFixture("capability-baseline");
    await rewriteArtifact(runtimeOfficial, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const room = entities.find(({ target }) => target === "representative-muc-room");
      if (!room) throw new Error("missing representative room fixture");
      (room.features as string[]).push("urn:xmpp:invented:0");
      (room.features as string[]).sort();
    });
    await expect(validate(runtimeOfficial)).rejects.toThrow(
      "outside the exact checked-in target registry",
    );

    const impossibleRoomMode = await completeFixture("capability-baseline");
    await rewriteArtifact(impossibleRoomMode, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const room = entities.find(({ target }) => target === "representative-muc-room");
      if (!room) throw new Error("missing representative room fixture");
      const features = room.features as string[];
      features.push(features.includes("muc_open") ? "muc_membersonly" : "muc_open");
      features.sort();
    });
    await expect(validate(impossibleRoomMode)).rejects.toThrow(
      "must match one complete runtime feature variant",
    );

    const partialCallMode = await completeFixture("capability-baseline");
    await rewriteArtifact(partialCallMode, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const server = entities.find(({ target }) => target === "server");
      if (!server) throw new Error("missing server fixture");
      const callFeatures = new Set([
        "urn:xmpp:jingle:1",
        "urn:xmpp:jingle:apps:rtp:1",
        "urn:xmpp:jingle:apps:rtp:audio",
        "urn:xmpp:jingle:apps:rtp:video",
        "urn:xmpp:jingle-message:0",
        "urn:xmpp:extdisco:2",
        "urn:waddle:transports:livekit:0",
        "urn:xmpp:jingle:muji:0",
      ]);
      server.features = (server.features as string[])
        .filter((feature) => !callFeatures.has(feature));
      (server.features as string[]).push("urn:xmpp:jingle:1");
      (server.features as string[]).sort();
    });
    await expect(validate(partialCallMode)).rejects.toThrow(
      "must match one complete runtime feature variant",
    );

    const syntheticUnion = await completeFixture("capability-baseline");
    await rewriteArtifact(syntheticUnion, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const extensions = entities.find(({ target }) => target === "extensions-service");
      if (!extensions) throw new Error("missing extensions target fixture");
      (extensions.features as string[]).push("urn:xmpp:ping");
      (extensions.features as string[]).sort();
    });
    await expect(validate(syntheticUnion)).rejects.toThrow(
      "synthetic union feature owned by server",
    );

    const unexpectedExtension = await completeFixture("capability-baseline");
    await rewriteArtifact(unexpectedExtension, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const extensions = entities.find(({ target }) => target === "extensions-service");
      if (!extensions) throw new Error("missing extensions target fixture");
      (extensions.features as string[]).push("urn:xmpp:invented:0");
      (extensions.features as string[]).sort();
    });
    await expect(validate(unexpectedExtension)).rejects.toThrow(
      "outside the exact checked-in target registry",
    );

    const concealedUnexpectedOfficial = await completeFixture("capability-baseline");
    await rewriteArtifact(concealedUnexpectedOfficial, "capability-reconciliation", (value) => {
      const checks = value.checks as Array<Record<string, unknown>>;
      const targetChecks = checks.flatMap(
        (check) => check.targets as Array<Record<string, unknown>>,
      );
      const server = targetChecks.find(({ target }) => target === "server");
      if (!server) throw new Error("missing server reconciliation fixture");
      (server.declaredFeatures as string[]).push("http://jabber.org/protocol/muc");
      (server.observedFeatures as string[]).push("http://jabber.org/protocol/muc");
      (server.declaredFeatures as string[]).sort();
      (server.observedFeatures as string[]).sort();
    });
    await expect(validate(concealedUnexpectedOfficial)).rejects.toThrow(
      "declaredFeatures must equal the sorted closed set",
    );

    const missingTarget = await completeFixture("capability-baseline");
    await rewriteArtifact(missingTarget, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      entities.splice(1, 1);
    });
    await expect(validate(missingTarget)).rejects.toThrow(
      "must account for every canonical target",
    );

    const relabelledWindow = await completeFixture("capability-baseline");
    await rewriteArtifact(relabelledWindow, "live-disco-export", (value) => {
      value.window = {
        start: "2026-07-10T08:00:00Z",
        end: "2026-07-10T10:00:00Z",
      };
    });
    await expect(validate(relabelledWindow)).rejects.toThrow(
      "window must match its artifact manifest entry",
    );

    const skippedClaimedTarget = await completeFixture("capability-baseline");
    await rewriteArtifact(skippedClaimedTarget, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const calls = entities.findIndex(({ target }) => target === "calls-mixer");
      entities.splice(calls, 1);
      (value.skippedTargets as Array<Record<string, unknown>>).push({
        target: "calls-mixer",
        reason: "not-configured",
  });
});
    await rewriteArtifact(
      skippedClaimedTarget,
      "capability-reconciliation",
      (value) => {
        (value.summary as Record<string, unknown>).observedTargetCount = 9;
      },
    );
    await expect(validate(skippedClaimedTarget)).rejects.toThrow(
      "cannot match a skipped or missing live target",
    );

    const inventedContract = await completeFixture("capability-baseline");
    await rewriteArtifact(inventedContract, "disco-target-contract", (value) => {
      const targets = value.targets as Array<Record<string, unknown>>;
      (targets[0].claimable_features as string[]).push("http://jabber.org/protocol/muc");
    });
    await expect(validate(inventedContract)).rejects.toThrow(
      "bytes must exactly match server/disco-target-contract.json",
    );

    const identityName = await completeFixture("capability-baseline");
    await rewriteArtifact(identityName, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      const identities = entities[0].identities as Array<Record<string, unknown>>;
      identities[0].name = "alice@example.test";
    });
    await expect(validate(identityName)).rejects.toThrow("must contain exactly");

    const unsafeFeature = await completeFixture("capability-baseline");
    await rewriteArtifact(unsafeFeature, "live-disco-export", (value) => {
      const entities = value.entities as Array<Record<string, unknown>>;
      (entities[0].features as string[]).push("https://evil.example/feature?jid=alice@example.test");
      (entities[0].features as string[]).sort();
    });
    await expect(validate(unsafeFeature)).rejects.toThrow("privacy-safe XMPP feature URI");
  });

});
