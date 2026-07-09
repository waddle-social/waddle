import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { lstat, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import {
  TRUSTED_ISSUER,
  TRUSTED_REPOSITORY,
  TRUSTED_SOURCE_REF,
  TRUSTED_WORKFLOW,
  TRUSTED_WORKFLOW_IDENTITY,
  githubAttestationArguments,
  parseLiveCollectionSubject,
  withPinnedAttestationCopies,
} from "../scripts/switchable-baseline/attestation";
import {
  parseReleaseArtifactProvenance,
  SERVER_PUBLICATION_WORKFLOW,
  serverReleaseArtifactSetSha256,
  verifyTrustedReleaseArtifactProvenance,
  WEB_PUBLICATION_WORKFLOW,
  webReleaseArtifactSetSha256,
} from "../scripts/switchable-baseline/release-artifact-provenance";
import { buildLiveCollectionSubject } from "../scripts/switchable-baseline/subject";
import { releaseArtifactProvenanceFixture } from "./support/release-artifact-provenance";
import {
  fixtureRoot,
  provenanceDigest,
  replicaProvenance,
  scope,
  serverCommit,
  webCommit,
  window,
  workflowCommit,
} from "./support/gate-evidence-hardening";

describe("Gate 0 attestation hardening", () => {
	test("builds a deterministic subject that binds protected sources and every live payload", async () => {
		const repositoryRoot = await fixtureRoot();
		const files = [
			"capability/live-disco-export.json",
			"prometheus/telemetry-baseline.json",
			"faro/browser-auth-bootstrap.json",
			"faro/browser-message-ack-latency.json",
			"faro/browser-session-lifecycle.json",
			"faro/browser-reconnect-duration.json",
		];
		for (const [index, suffix] of files.entries()) {
			const path = resolve(repositoryRoot, "target/switchable-baseline-inputs", suffix);
			await mkdir(dirname(path), { recursive: true });
			await Bun.write(path, `${JSON.stringify({ index }, null, 2)}\n`);
		}
		const { subject, path } = await buildLiveCollectionSubject({
			repositoryRoot,
			release: { serverCommit, webCommit },
			window,
			deploymentScope: scope,
			replicaProvenance,
			releaseArtifactProvenance: releaseArtifactProvenanceFixture(
				{ serverCommit, webCommit },
				replicaProvenance,
			),
			environment: {
				GITHUB_SHA: workflowCommit,
				GITHUB_REF: TRUSTED_SOURCE_REF,
				GITHUB_WORKFLOW_REF:
					`${TRUSTED_REPOSITORY}/.github/workflows/gate0-live-evidence.yml@${TRUSTED_SOURCE_REF}`,
				WADDLE_CAPABILITY_ENDPOINT: "wss://xmpp.waddle.social/xmpp-websocket",
				GRAFANA_PROMETHEUS_URL: "https://prometheus-prod.example/api/prom",
				PUBLIC_FARO_URL: "https://faro.example/collect/project",
				GRAFANA_FARO_QUERY_URL: "https://grafana.example/api/ds/query",
				GRAFANA_FARO_DATA_SOURCE_UID: "faro-query-source",
			},
		});
		expect(parseLiveCollectionSubject(await Bun.file(path).json())).toEqual(subject);
		expect(subject.artifacts).toHaveLength(6);
		expect(subject.sources.faroQuerySource.locatorSha256)
			.not.toBe(subject.sources.faroIngestLocatorSha256);
		expect(() => parseLiveCollectionSubject({
			...subject,
			replicaProvenance: {
				...replicaProvenance,
				deployment: { ...replicaProvenance.deployment, specReplicas: 3 },
			},
		})).toThrow("does not match the collected deployment scope");
		expect(() => parseLiveCollectionSubject({
			...subject,
			replicaProvenance: {
				...replicaProvenance,
				deployment: { ...replicaProvenance.deployment, uidSha256: "0".repeat(64) },
			},
		})).toThrow("placeholder digest");
		const selfHostedReplica: ReplicaProvenance = {
			schemaVersion: 1,
			kind: "self-hosted-config",
			deployment: {
				replicas: 2,
				configSha256: provenanceDigest("self-hosted deployment config"),
				operatorArtifactSha256: provenanceDigest("self-hosted operator artifact"),
			},
		};
		expect(() => parseLiveCollectionSubject({
			...subject,
			replicaProvenance: selfHostedReplica,
			releaseArtifactProvenance: releaseArtifactProvenanceFixture(
				{ serverCommit, webCommit },
				selfHostedReplica,
			),
		})).not.toThrow();
		expect(() => parseLiveCollectionSubject({
			...subject,
			replicaProvenance: {
				schemaVersion: 1,
				kind: "self-hosted-config",
				deployment: {
					replicas: 2,
					configSha256: provenanceDigest("self-hosted deployment config"),
				},
			},
		})).toThrow("must contain exactly");
		await expect(buildLiveCollectionSubject({
			repositoryRoot,
			release: { serverCommit, webCommit },
			window,
			deploymentScope: scope,
			replicaProvenance,
			releaseArtifactProvenance: releaseArtifactProvenanceFixture(
				{ serverCommit, webCommit },
				replicaProvenance,
			),
			environment: {
				GITHUB_SHA: workflowCommit,
				GITHUB_REF: TRUSTED_SOURCE_REF,
				GITHUB_WORKFLOW_REF:
					`${TRUSTED_REPOSITORY}/.github/workflows/gate0-live-evidence.yml@${TRUSTED_SOURCE_REF}`,
				WADDLE_CAPABILITY_ENDPOINT: "wss://xmpp.waddle.social/xmpp-websocket",
				GRAFANA_PROMETHEUS_URL: "https://prometheus-prod.example/api/prom",
				PUBLIC_FARO_URL: "https://faro.example/collect/project",
				GRAFANA_FARO_DATA_SOURCE_UID: "faro-query-source",
			},
		})).rejects.toThrow("GRAFANA_FARO_QUERY_URL");
		expect(subject.artifacts[0].sha256).toBe(
			createHash("sha256").update(`${JSON.stringify({ index: 0 }, null, 2)}\n`).digest("hex"),
		);
	});

	test("pins every GitHub attestation trust dimension in the production command", () => {
		const subject = parseLiveCollectionSubject({
			schemaVersion: 1,
			evidenceKind: "gate-0-live-collection-subject",
			release: { serverCommit, webCommit },
			window,
			deploymentScope: scope,
			replicaProvenance,
			releaseArtifactProvenance: releaseArtifactProvenanceFixture(
				{ serverCommit, webCommit },
				replicaProvenance,
			),
			attestor: {
				repository: TRUSTED_REPOSITORY,
				workflow: TRUSTED_WORKFLOW,
				issuer: TRUSTED_ISSUER,
				sourceRef: TRUSTED_SOURCE_REF,
				workflowCommit,
			},
			sources: {
				xmppLocatorSha256: "a".repeat(64),
				prometheusLocatorSha256: "b".repeat(64),
				faroIngestLocatorSha256: "c".repeat(64),
				faroQuerySource: {
					kind: "grafana-faro-query-api",
					locatorSha256: "4".repeat(64),
					dataSourceUidSha256: "5".repeat(64),
				},
			},
			artifacts: [
				"live-disco-export",
				"prometheus-baseline",
				"faro-browser-auth-bootstrap",
				"faro-browser-message-ack-latency",
				"faro-browser-session-lifecycle",
				"faro-browser-reconnect-duration",
			].map((role, index) => ({ role, sha256: String(index).repeat(64) })),
		});
		const command = githubAttestationArguments("subject.json", "bundle.json", subject);
		for (const pair of [
			["--repo", TRUSTED_REPOSITORY],
			["--signer-workflow", TRUSTED_WORKFLOW],
			["--cert-identity", TRUSTED_WORKFLOW_IDENTITY],
			["--cert-oidc-issuer", TRUSTED_ISSUER],
			["--source-ref", TRUSTED_SOURCE_REF],
			["--source-digest", workflowCommit],
			["--signer-digest", workflowCommit],
		] as const) {
			const index = command.indexOf(pair[0]);
			expect(command[index + 1]).toBe(pair[1]);
		}
		expect(command).toContain("--deny-self-hosted-runners");
		expect(command).toContain("--bundle");
	});

	test("binds the exact published and observed release artifact set and blocks unverified trust", async () => {
		expect(SERVER_PUBLICATION_WORKFLOW).toBe(
			"waddle-social/waddle/.github/workflows/gate0-server-release-artifacts.yml",
		);
		expect(WEB_PUBLICATION_WORKFLOW).toBe(
			"waddle-social/waddle/.github/workflows/gate0-web-release-artifacts.yml",
		);
		const release = { serverCommit, webCommit };
		const provenance = releaseArtifactProvenanceFixture(release, replicaProvenance);
		expect(parseReleaseArtifactProvenance(provenance, release, replicaProvenance))
			.toEqual(provenance);
		const reorderedArtifacts = {
			web: { ...provenance.artifacts.web },
			extensions: provenance.artifacts.extensions.map((extension) => ({
				digest: extension.digest,
				repository: extension.repository,
				name: extension.name,
			})),
			gitOps: { ...provenance.artifacts.gitOps },
			helmChart: { ...provenance.artifacts.helmChart },
			serverImage: { ...provenance.artifacts.serverImage },
		};
		expect(serverReleaseArtifactSetSha256(release, reorderedArtifacts))
			.toBe(provenance.publicationAttestations.server.artifactSetSha256);
		expect(webReleaseArtifactSetSha256(release, reorderedArtifacts))
			.toBe(provenance.publicationAttestations.web.artifactSetSha256);
		const changedWeb = structuredClone(provenance.artifacts);
		changedWeb.web.artifactSha256 = provenanceDigest("changed web artifact");
		expect(serverReleaseArtifactSetSha256(release, changedWeb))
			.toBe(provenance.publicationAttestations.server.artifactSetSha256);
		expect(webReleaseArtifactSetSha256(release, changedWeb))
			.not.toBe(provenance.publicationAttestations.web.artifactSetSha256);
		const mismatchedObserved = structuredClone(provenance);
		mismatchedObserved.observedDeployment.artifactDigests.serverImageDigest =
			`sha256:${provenanceDigest("other server image")}`;
		expect(() => parseReleaseArtifactProvenance(
			mismatchedObserved,
			release,
			replicaProvenance,
		)).toThrow("does not match the published release artifacts");
		const missingExtension = structuredClone(provenance);
		missingExtension.artifacts.extensions.pop();
		expect(() => parseReleaseArtifactProvenance(
			missingExtension,
			release,
			replicaProvenance,
		)).toThrow("exact canonical release extension order");
		const forgedPublication = structuredClone(provenance);
		forgedPublication.publicationAttestations.server.artifactSetSha256 =
			provenanceDigest("forged artifact set");
		expect(() => parseReleaseArtifactProvenance(
			forgedPublication,
			release,
			replicaProvenance,
		)).toThrow("artifactSetSha256");
		const wrongWebDeployment = structuredClone(provenance);
		wrongWebDeployment.observedWeb.webCommit = workflowCommit;
		expect(() => parseReleaseArtifactProvenance(
			wrongWebDeployment,
			release,
			replicaProvenance,
		)).toThrow("observedWeb.webCommit");
		await expect(verifyTrustedReleaseArtifactProvenance({ provenance, release }))
			.rejects.toThrow("release-artifact-provenance blocker");
	});

	test("verifies immutable attestation copies in a fresh private directory", async () => {
		const subjectBytes = new TextEncoder().encode("subject snapshot\n");
		const bundleBytes = new TextEncoder().encode("bundle snapshot\n");
		await withPinnedAttestationCopies(
			{ subjectBytes, bundleBytes },
			async ({ subjectPath, bundlePath }) => {
				expect(await Bun.file(subjectPath).text()).toBe("subject snapshot\n");
				expect(await Bun.file(bundlePath).text()).toBe("bundle snapshot\n");
				expect((await lstat(dirname(subjectPath))).mode & 0o777).toBe(0o700);
				expect((await lstat(subjectPath)).mode & 0o777).toBe(0o400);
				expect((await lstat(bundlePath)).mode & 0o777).toBe(0o400);
			},
		);
	});

});
