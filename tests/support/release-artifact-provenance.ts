import { createHash } from "node:crypto";
import type { EvidenceRelease } from "../../scripts/switchable-baseline/gate-evidence/common";
import type { ReplicaProvenance } from "../../scripts/switchable-baseline/replica-provenance";
import {
	GITOPS_REPOSITORY,
	HELM_CHART_REPOSITORY,
	RELEASE_EXTENSION_NAMES,
	RELEASE_PUBLICATION_ISSUER,
	RELEASE_PUBLICATION_REPOSITORY,
	RELEASE_PUBLICATION_SOURCE_REF,
	SERVER_IMAGE_REPOSITORY,
	SERVER_PUBLICATION_WORKFLOW,
	WEB_DEPLOYMENT_PROJECT,
	WEB_DEPLOYMENT_PROVIDER,
	WEB_PUBLICATION_WORKFLOW,
	serverReleaseArtifactSetSha256,
	webReleaseArtifactSetSha256,
	type ReleaseArtifactProvenance,
} from "../../scripts/switchable-baseline/release-artifact-provenance";

function sha256(value: string): string {
	return createHash("sha256").update(value).digest("hex");
}

function ociDigest(value: string): string {
	return `sha256:${sha256(value)}`;
}

export function releaseArtifactProvenanceFixture(
	release: EvidenceRelease,
	replicaProvenance: ReplicaProvenance,
): ReleaseArtifactProvenance {
	const artifacts: ReleaseArtifactProvenance["artifacts"] = {
		serverImage: {
			repository: SERVER_IMAGE_REPOSITORY,
			digest: ociDigest("server-image"),
		},
		helmChart: {
			repository: HELM_CHART_REPOSITORY,
			digest: ociDigest("helm-chart"),
		},
		gitOps: {
			repository: GITOPS_REPOSITORY,
			digest: ociDigest("gitops"),
		},
		extensions: RELEASE_EXTENSION_NAMES.map((name) => ({
			name,
			repository: `${SERVER_IMAGE_REPOSITORY}/extensions/${name}`,
			digest: ociDigest(`extension:${name}`),
		})),
		web: {
			artifactSha256: sha256("web artifact"),
			deploymentIdentitySha256: sha256("cloudflare deployment identity"),
		},
	};
	const artifactDigests = {
		serverImageDigest: artifacts.serverImage.digest,
		helmChartDigest: artifacts.helmChart.digest,
		gitOpsDigest: artifacts.gitOps.digest,
		extensions: artifacts.extensions.map(({ name, digest }) => ({ name, digest })),
	};
	const observedDeployment: ReleaseArtifactProvenance["observedDeployment"] =
		replicaProvenance.kind === "kubernetes-deployment"
			? {
				kind: "kubernetes-flux",
				deployment: {
					...replicaProvenance.deployment,
					updatedReplicas: replicaProvenance.deployment.specReplicas,
					readyReplicas: replicaProvenance.deployment.specReplicas,
					availableReplicas: replicaProvenance.deployment.specReplicas,
				},
				flux: {
					gitOpsSource: {
						apiVersion: "source.toolkit.fluxcd.io/v1",
						kind: "OCIRepository",
						name: "waddle-server",
						namespace: "flux-system",
						uidSha256: sha256("flux-system/OCIRepository/waddle-server/uid"),
						generation: 8,
						observedGeneration: 8,
						ready: true,
						artifactDigest: artifacts.gitOps.digest,
					},
					kustomization: {
						apiVersion: "kustomize.toolkit.fluxcd.io/v1",
						kind: "Kustomization",
						name: "infra-waddle-server",
						namespace: "flux-system",
						uidSha256: sha256("flux-system/Kustomization/infra-waddle-server/uid"),
						generation: 9,
						observedGeneration: 9,
						ready: true,
						sourceArtifactDigest: artifacts.gitOps.digest,
					},
					chartSource: {
						apiVersion: "source.toolkit.fluxcd.io/v1",
						kind: "OCIRepository",
						name: "waddle-server-chart",
						namespace: "waddle",
						uidSha256: sha256("waddle/OCIRepository/waddle-server-chart/uid"),
						generation: 10,
						observedGeneration: 10,
						ready: true,
						artifactDigest: artifacts.helmChart.digest,
					},
					helmRelease: {
						apiVersion: "helm.toolkit.fluxcd.io/v2",
						kind: "HelmRelease",
						name: "waddle-server",
						namespace: "waddle",
						uidSha256: sha256("waddle/HelmRelease/waddle-server/uid"),
						generation: 11,
						observedGeneration: 11,
						ready: true,
					},
				},
				artifactDigests,
			}
			: {
				kind: "self-hosted",
				deployment: replicaProvenance.deployment,
				artifactDigests,
			};
	return {
		schemaVersion: 1,
		release,
		artifacts,
		observedDeployment,
		observedWeb: {
			provider: WEB_DEPLOYMENT_PROVIDER,
			project: WEB_DEPLOYMENT_PROJECT,
			artifactSha256: artifacts.web.artifactSha256,
			deploymentIdentitySha256: artifacts.web.deploymentIdentitySha256,
			webCommit: release.webCommit,
		},
		publicationAttestations: {
			server: {
				kind: "github-sigstore",
				repository: RELEASE_PUBLICATION_REPOSITORY,
				workflow: SERVER_PUBLICATION_WORKFLOW,
				issuer: RELEASE_PUBLICATION_ISSUER,
				sourceRef: RELEASE_PUBLICATION_SOURCE_REF,
				workflowCommit: release.serverCommit,
				artifactSetSha256: serverReleaseArtifactSetSha256(release, artifacts),
				subjectSha256: sha256("server publication subject"),
				bundleSha256: sha256("server publication bundle"),
			},
			web: {
				kind: "github-sigstore",
				repository: RELEASE_PUBLICATION_REPOSITORY,
				workflow: WEB_PUBLICATION_WORKFLOW,
				issuer: RELEASE_PUBLICATION_ISSUER,
				sourceRef: RELEASE_PUBLICATION_SOURCE_REF,
				workflowCommit: release.webCommit,
				artifactSetSha256: webReleaseArtifactSetSha256(release, artifacts),
				subjectSha256: sha256("web publication subject"),
				bundleSha256: sha256("web publication bundle"),
			},
		},
	};
}
