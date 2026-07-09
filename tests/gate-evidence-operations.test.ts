import { describe, expect, test } from "bun:test";
import {
  lstat,
  mkdir,
  mkdtemp,
  readdir,
  rm,
  symlink,
	unlink,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  requireArtifactManifestForCompleteGateZeroRecord,
  requireSharedGateZeroRelease,
  resolveTrustedEvidenceFile,
  validateArtifactManifestReference,
} from "./support/gate-evidence";
import { parseBaselineCatalog } from "../scripts/switchable-baseline/catalog";
import { normalizeFaroAggregateExport } from "../scripts/switchable-baseline/faro";
import {
  ensureSafeOutputParent,
  resolveRestrictedFaroInput,
} from "../scripts/switchable-baseline/filesystem";
import { commitFilesNoClobber } from "../scripts/switchable-baseline/no-clobber";
import {
  completeFixture,
  fixtureRoot,
  otherServerCommit,
  otherWebCommit,
  release,
  rewriteArtifact,
  serialize,
  serverCommit,
  sha256,
  trackTemporaryRoot,
  validate,
  webCommit,
  window,
  writeFixtureFile,
} from "./support/gate-evidence-fixtures";

describe("Gate 0 evidence trust and publication", () => {
  test("normalizes only typed Faro aggregates with exact provenance filters", async () => {
    const fixture = await completeFixture("telemetry-baseline");
    const artifact = fixture.contents.get("faro-browser-message-ack-latency");
    if (!artifact) throw new Error("missing Faro normalizer fixture");
    const signal = parseBaselineCatalog(
      await Bun.file(resolve(fixture.root, "docs/observability/switchable-baseline-signals.json")).json(),
    ).signals.find(({ id }) => id === "browser-message-ack-latency");
    if (!signal) throw new Error("missing Faro normalizer signal");
    const raw = serialize({
      schemaVersion: 1,
      query: (artifact.source as Record<string, unknown>).query,
      source: { sourceId: "waddle-chat" },
      rows: artifact.series,
    });
    const normalized = normalizeFaroAggregateExport(raw, signal, {
      webCommit,
      deploymentEnvironment: "production",
      cluster: "waddle-cloud",
      namespace: "waddle",
      window,
    });
    expect(normalized.source.rowCount).toBe(2);
    expect(normalized.source.rawSha256).toBe(sha256(raw));

    const wrong = JSON.parse(raw) as Record<string, unknown>;
    ((wrong.query as Record<string, unknown>).window as Record<string, unknown>).end =
      "2026-07-10T10:01:00Z";
    expect(() => normalizeFaroAggregateExport(serialize(wrong), signal, {
      webCommit,
      deploymentEnvironment: "production",
      cluster: "waddle-cloud",
      namespace: "waddle",
      window,
    })).toThrow("exact release/deployment/window plan");
  });

  test("binds Faro queries to source, web release, environment, cluster, and namespace without a Prometheus job", async () => {
    const fixture = await completeFixture("telemetry-baseline");
    const artifact = fixture.contents.get("faro-browser-message-ack-latency");
    const signal = parseBaselineCatalog(
      await Bun.file(resolve(fixture.root, "docs/observability/switchable-baseline-signals.json")).json(),
    ).signals.find(({ id }) => id === "browser-message-ack-latency");
    if (!artifact || !signal) throw new Error("missing Faro provenance fixture");
    const base = {
      schemaVersion: 1,
      query: structuredClone((artifact.source as Record<string, unknown>).query),
      source: { sourceId: "waddle-chat" },
      rows: artifact.series,
    };
    const context = {
      webCommit,
      deploymentEnvironment: "production",
      cluster: "waddle-cloud",
      namespace: "waddle",
      window,
    };
    expect(() => normalizeFaroAggregateExport(serialize(base), signal, context))
      .not.toThrow();

    for (const [key, value] of [
      ["sourceId", "other-chat"],
      ["release", otherWebCommit],
      ["deploymentEnvironment", "staging"],
      ["cluster", "other-cluster"],
      ["namespace", "other-namespace"],
    ] as const) {
      const changed = structuredClone(base);
      (changed.query as Record<string, unknown>)[key] = value;
      expect(() => normalizeFaroAggregateExport(serialize(changed), signal, context))
        .toThrow("exact release/deployment/window plan");
    }

    const wrongSource = structuredClone(base);
    (wrongSource.source as Record<string, unknown>).sourceId = "other-chat";
    expect(() => normalizeFaroAggregateExport(serialize(wrongSource), signal, context))
      .toThrow("bounded catalog source id");

    const prometheusJob = structuredClone(base);
    (prometheusJob.query as Record<string, unknown>).job = "waddle-server";
    expect(() => normalizeFaroAggregateExport(serialize(prometheusJob), signal, context))
      .toThrow("exact release/deployment/window plan");
  });

  test("staging paths use real files, reject symlinks, and publish without clobber", async () => {
    const root = await fixtureRoot();
    const evidenceRoot = resolve(root, "docs/evidence");
    const secureRoot = trackTemporaryRoot(
		await mkdtemp(join(tmpdir(), "waddle-faro-secure-")),
	);
    const input = resolve(secureRoot, "aggregate.json");
    await Bun.write(input, "{}\n");
    expect(await resolveRestrictedFaroInput(input, evidenceRoot)).toBe(input);

    const inside = resolve(evidenceRoot, "raw.json");
    await Bun.write(inside, "{}\n");
    await expect(resolveRestrictedFaroInput(inside, evidenceRoot))
      .rejects.toThrow("outside docs/evidence");

    const linkedInput = resolve(secureRoot, "linked.json");
    await symlink(input, linkedInput);
    await expect(resolveRestrictedFaroInput(linkedInput, evidenceRoot))
      .rejects.toThrow("must not be a symlink");

    const output = resolve(
      root,
      "target/switchable-baseline-inputs/faro/browser-auth-bootstrap.json",
    );
    await ensureSafeOutputParent(root, output);
    await commitFilesNoClobber(
      root,
      [{ path: output, contents: "first\n" }],
      async () => undefined,
    );
    await expect(commitFilesNoClobber(
      root,
      [{ path: output, contents: "second\n" }],
      async () => undefined,
    )).rejects.toThrow("refuses to replace existing output");
    expect(await Bun.file(output).text()).toBe("first\n");
    expect((await lstat(output)).isFile()).toBeTrue();
    expect((await readdir(resolve(root, "target/switchable-baseline-inputs/faro")))
      .filter((name) => name.endsWith(".tmp"))).toEqual([]);

		await rm(output);
		await expect(commitFilesNoClobber(
			root,
			[{ path: output, contents: "transaction\n" }],
			async () => {
				await unlink(output);
				await Bun.write(output, "replacement\n");
				throw new Error("reject transaction");
			},
		)).rejects.toThrow("reject transaction");
		expect(await Bun.file(output).text()).toBe("replacement\n");

    const target = resolve(secureRoot, "target.json");
    await Bun.write(target, "target\n");
    await rm(output);
    await symlink(target, output);
    await expect(ensureSafeOutputParent(root, output))
      .rejects.toThrow("output must not be a symlink");
    await expect(commitFilesNoClobber(
      root,
      [{ path: output, contents: "unsafe\n" }],
      async () => undefined,
    ))
      .rejects.toThrow("output must not be a symlink");
    expect(await Bun.file(target).text()).toBe("target\n");

    const linkedParentTarget = resolve(root, "linked-parent-target");
    await mkdir(linkedParentTarget);
    const linkedParent = resolve(root, "target/switchable-baseline-inputs/linked");
    await symlink(linkedParentTarget, linkedParent);
    await expect(ensureSafeOutputParent(root, resolve(linkedParent, "artifact.json")))
      .rejects.toThrow("parents must not contain symlinks");
  });

  test("rejects free-form Prometheus note and conclusion payloads", async () => {
    const note = await completeFixture("telemetry-baseline");
    await rewriteArtifact(note, "prometheus-baseline", (value) => {
      (value.manualFaro as Record<string, unknown>).note = "raw session alice@example.test";
    });
    await expect(validate(note)).rejects.toThrow("fixed privacy-reviewed text");

    const conclusion = await completeFixture("telemetry-baseline");
    await rewriteArtifact(conclusion, "prometheus-baseline", (value) => {
      value.conclusion = "raw export secret-session";
    });
    await expect(validate(conclusion)).rejects.toThrow("fixed privacy-reviewed text");
  });

  test("requires complete Gate 0 capability and telemetry manifests to share a release", async () => {
    const root = await fixtureRoot();
    const capability = await completeFixture("capability-baseline", root, release);
    const telemetry = await completeFixture("telemetry-baseline", root, release);
    const validatedCapability = await validate(capability);
    const validatedTelemetry = await validate(telemetry);
    expect(() => requireSharedGateZeroRelease([
      validatedCapability,
      validatedTelemetry,
    ])).not.toThrow();

    const otherRoot = await fixtureRoot();
    const otherTelemetry = await completeFixture("telemetry-baseline", otherRoot, {
      serverCommit: otherServerCommit,
      webCommit: otherWebCommit,
    });
    const validatedOtherTelemetry = await validate(otherTelemetry);
    expect(() => requireSharedGateZeroRelease([
      validatedCapability,
      validatedOtherTelemetry,
    ])).toThrow("must share one release tuple");
  });

  test("seals the complete Gate 0 directory to the canonical generated package", async () => {
    const root = await fixtureRoot();
    const capability = await completeFixture("capability-baseline", root, release);
    const telemetry = await completeFixture("telemetry-baseline", root, release);
    const validated = [await validate(capability), await validate(telemetry)];
    expect(() => requireSharedGateZeroRelease(validated)).not.toThrow();

    await Bun.write(resolve(root, "docs/evidence/gate-0/raw-faro-export.json"), "{}\n");
    expect(() => requireSharedGateZeroRelease(validated))
      .toThrow("unexpected: docs/evidence/gate-0/raw-faro-export.json");
    await rm(resolve(root, "docs/evidence/gate-0/raw-faro-export.json"));

    const review = resolve(root, "docs/evidence/gate-0/telemetry-baseline.md");
    await Bun.write(review, "hand-edited review\n");
    expect(() => requireSharedGateZeroRelease(validated))
      .toThrow("review must exactly match");
    await rm(review);
    expect(() => requireSharedGateZeroRelease(validated))
      .toThrow("generated Gate 0 review does not exist");
  });

  test("rejects path traversal, symlinked trust roots and artifacts, and SHA drift", async () => {
    const traversal = await completeFixture("capability-baseline");
    await expect(validateArtifactManifestReference(
      traversal.root,
      {
        type: "artifact-manifest",
        path: "docs/evidence/../observability/switchable-baseline-signals.json",
        sha256: "a".repeat(64),
      },
      "capability-baseline",
    )).rejects.toThrow("under docs/evidence");

    const linkedArtifact = await completeFixture("capability-baseline");
    const targetPath = linkedArtifact.artifacts[0].path + ".target";
    await writeFixtureFile(
      linkedArtifact.root,
      targetPath,
      serialize(linkedArtifact.contents.get("live-disco-export")),
    );
    await rm(resolve(linkedArtifact.root, linkedArtifact.artifacts[0].path));
    await symlink(
      resolve(linkedArtifact.root, targetPath),
      resolve(linkedArtifact.root, linkedArtifact.artifacts[0].path),
    );
    await expect(validate(linkedArtifact)).rejects.toThrow("must not contain symlinks");

    const tampered = await completeFixture("capability-baseline");
    await Bun.write(resolve(tampered.root, tampered.artifacts[0].path), "tampered\n");
    await expect(validate(tampered)).rejects.toThrow("SHA-256 does not match");

    const report = await completeFixture("capability-baseline");
    const reportPath = "docs/evidence/gate-0/review.md";
    await writeFixtureFile(report.root, reportPath, "# review\n");
    expect(resolveTrustedEvidenceFile(report.root, reportPath, "manual report", ".md"))
      .toBe(resolve(report.root, reportPath));

    const linkedRoot = trackTemporaryRoot(
		await mkdtemp(join(tmpdir(), "waddle-linked-evidence-root-")),
	);
    await mkdir(resolve(linkedRoot, "docs"), { recursive: true });
    await mkdir(resolve(linkedRoot, "evidence-target"), { recursive: true });
    await Bun.write(resolve(linkedRoot, "evidence-target/review.md"), "# linked\n");
    await symlink(resolve(linkedRoot, "evidence-target"), resolve(linkedRoot, "docs/evidence"));
    expect(() => resolveTrustedEvidenceFile(
      linkedRoot,
      "docs/evidence/review.md",
      "manual report",
      ".md",
    )).toThrow("trust root must not");

    expect(() => resolveTrustedEvidenceFile(
      report.root,
      "docs/evidence/../observability/report.md",
      "manual report",
      ".md",
    )).toThrow("under docs/evidence");
  });

  test("does not let repository tests or Markdown reports complete capability or telemetry evidence", () => {
    for (const kind of ["capability-baseline", "telemetry-baseline"] as const) {
      expect(() => requireArtifactManifestForCompleteGateZeroRecord({
        kind,
        status: "complete",
        reference: {
          type: "repo-test",
          path: "tests/critical-journeys.test.ts",
          testId: "covers every release dimension and named critical journey",
        },
      })).toThrow("must use an artifact-manifest reference");
      expect(() => requireArtifactManifestForCompleteGateZeroRecord({
        kind,
        status: "complete",
        reference: {
          type: "manual-report",
          path: "docs/evidence/gate-0/report.md",
          sha256: "a".repeat(64),
        },
      })).toThrow("must use an artifact-manifest reference");
	}
	});
});
