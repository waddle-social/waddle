import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import {
  CRITICAL_JOURNEY_CONTRACT_PATH,
  validateCriticalJourneyContract,
} from "../scripts/switchable-baseline/critical-journey-contract";
import {
  buildJourneyBaselineManifest,
  CRITICAL_JOURNEY_VALIDATOR_PATHS,
  validateJourneyBaselineManifestReference,
} from "../scripts/switchable-baseline/journey-baseline";
import {
  scenarioTestId,
  validateJourneyEvidenceReference,
} from "../scripts/switchable-baseline/journey-evidence-reference";
import { validatePerformanceEnvironment } from "../scripts/switchable-baseline/journey-performance-environment";
import { requireCompleteEvidenceReferencePolicy } from "./support/switchable-program";
import {
  fixtureRoot,
  scope,
  serverCommit,
  webCommit,
  window,
  workflowCommit,
} from "./support/gate-evidence-hardening";

describe("Gate 0 journey-evidence hardening", () => {
	test("does not let the old generic test complete journey-baseline", () => {
		expect(() => requireCompleteEvidenceReferencePolicy("0", {
			kind: "journey-baseline",
			status: "complete",
			reference: {
				type: "repo-test",
				path: "tests/critical-journeys.test.ts",
				testId: "covers every release dimension and named critical journey",
			},
		})).toThrow("typed immutable manifest");
	});

	test("lets only release-bound durable journey evidence affect readiness", async () => {
		const repositoryRoot = await fixtureRoot();
		const binding = {
			scenarioId: "authenticate/valid-login/desktop-web/local",
			client: "desktop-web",
			topology: "local",
			kind: "e2e",
		};
		const testId = scenarioTestId(binding);
		const testPath = "tests/switchable-authenticate.test.ts";
		await mkdir(resolve(repositoryRoot, "tests"), { recursive: true });
		await Bun.write(resolve(repositoryRoot, testPath), `test("${testId}", () => {});\n`);
		const sourceAtCommit = async (root: string, commit: string, sourcePath: string) => {
			if (commit !== serverCommit) throw new Error("unexpected commit");
			return Bun.file(resolve(root, sourcePath)).bytes();
		};
		const context = {
			repositoryRoot,
			release: {
				contractCommit: serverCommit,
				serverCommit,
				webCommit,
				clientCommit: webCommit,
			},
			sourceAtCommit,
			gateId: "3" as const,
			kind: "e2e",
			status: "partial" as const,
			binding,
		};
		const repoTest = { type: "repo-test", path: testPath, testId, ...binding };
		await expect(validateJourneyEvidenceReference(repoTest, context)).resolves.toBeUndefined();
			await expect(validateJourneyEvidenceReference(repoTest, {
				...context,
				status: "complete",
			})).rejects.toThrow("verified passing-run or manual/live evidence attestation");
		await expect(validateJourneyEvidenceReference({
			type: "ci-run",
			url: "https://github.com/waddle-social/waddle/actions/runs/12345",
			commit: serverCommit,
			}, {
				...context,
				status: "complete",
			})).rejects.toThrow("verified passing-run or manual/live evidence attestation");

		const schema = "switchable-evidence/gate-3/e2e/v1";
		const artifactPath = `docs/evidence/gate-3/journeys/${binding.scenarioId}/e2e.json`;
		const artifact = {
			schemaVersion: 1,
			schema,
			evidenceKind: "e2e",
			status: "complete",
			scope: {
				type: "scenario",
				scenarioId: binding.scenarioId,
				client: binding.client,
				topology: binding.topology,
			},
			release: { serverCommit, webCommit, appCommit: webCommit },
			window,
			capturedAt: "2026-07-10T10:05:00Z",
			assertions: [{ id: "journey-complete", status: "pass", observed: true }],
		};
		const writeArtifact = async (value: unknown) => {
			const contents = `${JSON.stringify(value, null, 2)}\n`;
			await mkdir(dirname(resolve(repositoryRoot, artifactPath)), { recursive: true });
			await Bun.write(resolve(repositoryRoot, artifactPath), contents);
			return createHash("sha256").update(contents).digest("hex");
		};
		const manualReference = {
			type: "manual-schema",
			path: artifactPath,
			schema,
			sha256: await writeArtifact(artifact),
			...binding,
		};
		await expect(validateJourneyEvidenceReference(manualReference, {
			...context,
			status: "partial",
		})).resolves.toBeUndefined();
			await expect(validateJourneyEvidenceReference(manualReference, {
				...context,
				status: "complete",
			})).rejects.toThrow("verified passing-run or manual/live evidence attestation");
		manualReference.sha256 = await writeArtifact({
			...artifact,
			release: { serverCommit: workflowCommit, webCommit, appCommit: webCommit },
		});
		await expect(validateJourneyEvidenceReference(manualReference, {
			...context,
			status: "partial",
		})).rejects.toThrow("does not match the immutable journey release");

		const profilePath = "docs/product/performance-profile.json";
		await mkdir(dirname(resolve(repositoryRoot, profilePath)), { recursive: true });
		await Bun.write(
			resolve(repositoryRoot, profilePath),
			await Bun.file(resolve(import.meta.dir, "..", profilePath)).bytes(),
		);
		const performance = {
			profileId: "switchable-500-v1",
			client: "desktop-web",
			hardware: "Mac mini M1 2020",
			ramGiB: 8,
			network: { downMbps: 10, upMbps: 2, roundTripMs: 80 },
			osVersion: "macOS 15.5",
			appCommit: webCommit,
			browserVersion: "Chrome/138.0.0",
		};
		await expect(validatePerformanceEnvironment(performance, {
			...binding,
			kind: "performance",
		}, {
			repositoryRoot,
			contractCommit: serverCommit,
			expectedAppCommit: webCommit,
			sourceAtCommit,
		})).resolves.toBeUndefined();
		await expect(validatePerformanceEnvironment({
			...performance,
			appCommit: workflowCommit,
		}, { ...binding, kind: "performance" }, {
			repositoryRoot,
			contractCommit: serverCommit,
			expectedAppCommit: webCommit,
			sourceAtCommit,
		})).rejects.toThrow("does not match its client release");
	});

	test("accepts only a hashed journey manifest bound to deterministic production validation", async () => {
		const repositoryRoot = await fixtureRoot();
		const sourceRoot = resolve(import.meta.dir, "..");
		for (const path of [CRITICAL_JOURNEY_CONTRACT_PATH, ...CRITICAL_JOURNEY_VALIDATOR_PATHS]) {
			const destination = resolve(repositoryRoot, path);
			await mkdir(dirname(destination), { recursive: true });
			await Bun.write(destination, await Bun.file(resolve(sourceRoot, path)).bytes());
		}
		const contractBytes = await Bun.file(
			resolve(repositoryRoot, "docs/product/critical-journeys.json"),
		).bytes();
		const contract = JSON.parse(new TextDecoder().decode(contractBytes)) as unknown;
		const summary = await validateCriticalJourneyContract(contract);
		const changedOwner = structuredClone(contract) as {
			journeys: Array<{ owner: string; evidence: { defaultStatus: string } }>;
		};
		changedOwner.journeys[0].owner = "unbounded-owner";
		await expect(validateCriticalJourneyContract(changedOwner))
			.rejects.toThrow("owner and gate");
		const changedStatus = structuredClone(contract) as {
			journeys: Array<{ owner: string; evidence: { defaultStatus: string } }>;
		};
		changedStatus.journeys[0].evidence.defaultStatus = "complete";
		await expect(validateCriticalJourneyContract(changedStatus))
			.rejects.toThrow("derived from its full matrix");
		const selfAsserted = structuredClone(contract) as {
			journeys: Array<{
				id: string;
				clients: string[];
				topologies: string[];
				requirements: Array<{ id: string }>;
				evidence: {
					defaultStatus: string;
					records: unknown[];
				};
			}>;
		};
		const firstJourney = selfAsserted.journeys[0];
		const scenarioId = `${firstJourney.id}/${firstJourney.requirements[0].id}/`
			+ `${firstJourney.clients[0]}/${firstJourney.topologies[0]}`;
		firstJourney.evidence.defaultStatus = "partial";
		firstJourney.evidence.records = [{
			scenarioId,
			kind: "e2e",
			status: "complete",
			reference: { type: "self-asserted-success" },
		}];
		await expect(validateCriticalJourneyContract(selfAsserted, {
			repositoryRoot,
			release: {
				contractCommit: serverCommit,
				serverCommit,
				webCommit,
				clientCommits: {
					"desktop-web": webCommit,
					"android-pwa": webCommit,
					ios: serverCommit,
					macos: serverCommit,
				},
			},
			sourceAtCommit: async (root, commit, sourcePath) => {
				if (commit !== serverCommit) throw new Error("unexpected commit");
				return Bun.file(resolve(root, sourcePath)).bytes();
			},
		})).rejects.toThrow("unsupported journey evidence reference type");
		expect(summary.scenarioCount).toBe(359);
		const path = resolve(repositoryRoot, "docs/evidence/journey-baseline.manifest.json");
		const sourceReader = async (root: string, commit: string, sourcePath: string) => {
			if (commit !== serverCommit) throw new Error("unexpected commit");
			return Bun.file(resolve(root, sourcePath)).bytes();
		};
		const reference = await buildJourneyBaselineManifest(
			repositoryRoot,
			serverCommit,
			sourceReader,
		);
		const manifest = await Bun.file(path).json() as {
			summary: { matrixSha256: string };
		};
		await expect(validateJourneyBaselineManifestReference(
			repositoryRoot,
			reference,
			sourceReader,
		)).resolves.toBeUndefined();

		const tampered = structuredClone(manifest);
		tampered.summary.matrixSha256 = "f".repeat(64);
		const tamperedContents = `${JSON.stringify(tampered, null, 2)}\n`;
		await Bun.write(path, tamperedContents);
		await expect(validateJourneyBaselineManifestReference(
			repositoryRoot,
			{
				type: "journey-baseline-manifest",
				path: "docs/evidence/journey-baseline.manifest.json",
				sha256: createHash("sha256").update(tamperedContents).digest("hex"),
			},
			sourceReader,
		)).rejects.toThrow("deterministic validation");
	});
});
