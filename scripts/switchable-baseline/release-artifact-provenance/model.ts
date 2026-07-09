import type { EvidenceRelease } from "../gate-evidence/common";

export const SERVER_IMAGE_REPOSITORY = "ghcr.io/waddle-social/waddle";
export const HELM_CHART_REPOSITORY =
  "ghcr.io/waddle-social/waddle/charts/waddle-server";
export const GITOPS_REPOSITORY =
  "ghcr.io/waddle-social/waddle/gitops-waddle-server";
export const WEB_DEPLOYMENT_PROVIDER = "cloudflare-workers";
export const WEB_DEPLOYMENT_PROJECT = "waddle-chat";
export const RELEASE_PUBLICATION_REPOSITORY = "waddle-social/waddle";
export const RELEASE_PUBLICATION_ISSUER =
  "https://token.actions.githubusercontent.com";
export const RELEASE_PUBLICATION_SOURCE_REF = "refs/heads/main";
export const SERVER_PUBLICATION_WORKFLOW =
  "waddle-social/waddle/.github/workflows/gate0-server-release-artifacts.yml";
export const WEB_PUBLICATION_WORKFLOW =
  "waddle-social/waddle/.github/workflows/gate0-web-release-artifacts.yml";

export const RELEASE_EXTENSION_NAMES = [
  "link-board",
  "ai-chatbot",
  "decision-polls",
  "github",
  "stargate-quotes",
] as const;

export type ReleaseExtensionName = (typeof RELEASE_EXTENSION_NAMES)[number];

export interface OciArtifact {
  repository: string;
  digest: string;
}

export interface ExtensionArtifact extends OciArtifact {
  name: ReleaseExtensionName;
}

export interface ReleaseArtifacts {
  serverImage: OciArtifact;
  helmChart: OciArtifact;
  gitOps: OciArtifact;
  extensions: ExtensionArtifact[];
  web: {
    artifactSha256: string;
    deploymentIdentitySha256: string;
  };
}

export interface ObservedArtifactDigests {
  serverImageDigest: string;
  helmChartDigest: string;
  gitOpsDigest: string;
  extensions: Array<{ name: ReleaseExtensionName; digest: string }>;
}

export interface ObservedFluxObject {
  apiVersion: string;
  kind: string;
  name: string;
  namespace: string;
  uidSha256: string;
  generation: number;
  observedGeneration: number;
  ready: true;
}

export interface PublicationAttestation {
  kind: "github-sigstore";
  repository: typeof RELEASE_PUBLICATION_REPOSITORY;
  workflow: string;
  issuer: typeof RELEASE_PUBLICATION_ISSUER;
  sourceRef: typeof RELEASE_PUBLICATION_SOURCE_REF;
  workflowCommit: string;
  artifactSetSha256: string;
  subjectSha256: string;
  bundleSha256: string;
}

export type ObservedReleaseDeployment =
  | {
    kind: "kubernetes-flux";
    deployment: {
      apiVersion: "apps/v1";
      name: string;
      namespace: string;
      uidSha256: string;
      generation: number;
      observedGeneration: number;
      specReplicas: number;
      updatedReplicas: number;
      readyReplicas: number;
      availableReplicas: number;
      configSha256: string;
    };
    flux: {
      gitOpsSource: ObservedFluxObject & { artifactDigest: string };
      kustomization: ObservedFluxObject & { sourceArtifactDigest: string };
      chartSource: ObservedFluxObject & { artifactDigest: string };
      helmRelease: ObservedFluxObject;
    };
    artifactDigests: ObservedArtifactDigests;
  }
  | {
    kind: "self-hosted";
    deployment: {
      replicas: number;
      configSha256: string;
      operatorArtifactSha256: string;
    };
    artifactDigests: ObservedArtifactDigests;
  };

export interface ReleaseArtifactProvenance {
  schemaVersion: 1;
  release: EvidenceRelease;
  artifacts: ReleaseArtifacts;
  observedDeployment: ObservedReleaseDeployment;
  observedWeb: {
    provider: typeof WEB_DEPLOYMENT_PROVIDER;
    project: typeof WEB_DEPLOYMENT_PROJECT;
    artifactSha256: string;
    deploymentIdentitySha256: string;
    webCommit: string;
  };
  publicationAttestations: {
    server: PublicationAttestation;
    web: PublicationAttestation;
  };
}

export type ReleaseArtifactProvenanceVerifier = (input: {
  provenance: ReleaseArtifactProvenance;
  release: EvidenceRelease;
}) => Promise<void>;
