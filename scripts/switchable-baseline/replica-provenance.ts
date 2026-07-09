import {
	boundedLabelPattern,
	requireExactKeys,
	requireInteger,
	requireRecord,
	requireSha256,
	requireString,
} from "./gate-evidence/common";

export type ReplicaProvenance =
	| {
		schemaVersion: 1;
		kind: "kubernetes-deployment";
		deployment: {
			apiVersion: "apps/v1";
			name: string;
			namespace: string;
			uidSha256: string;
			generation: number;
			observedGeneration: number;
			specReplicas: number;
			configSha256: string;
		};
	}
	| {
		schemaVersion: 1;
		kind: "self-hosted-config";
		deployment: {
			replicas: number;
			configSha256: string;
			operatorArtifactSha256: string;
		};
	};

function boundedLabel(value: unknown, label: string): string {
	if (typeof value !== "string" || value === "unknown" || !boundedLabelPattern.test(value)) {
		throw new Error(`${label} must be a bounded deployment label`);
	}
	return value;
}

function requireProvenanceSha256(value: unknown, label: string): string {
	const digest = requireSha256(value, label);
	if (/^([0-9a-f])\1{63}$/.test(digest)) {
		throw new Error(`${label} must not be a placeholder digest`);
	}
	return digest;
}

export function parseReplicaProvenance(
	value: unknown,
	scope: { namespace: string; expectedReplicas: number },
): ReplicaProvenance {
	const provenance = requireRecord(value, "replica provenance");
	requireExactKeys(provenance, ["schemaVersion", "kind", "deployment"], "replica provenance");
	if (provenance.schemaVersion !== 1) throw new Error("replica provenance schemaVersion must be 1");
	const deployment = requireRecord(provenance.deployment, "replica provenance.deployment");
	if (provenance.kind === "kubernetes-deployment") {
		requireExactKeys(deployment, [
			"apiVersion", "name", "namespace", "uidSha256", "generation", "observedGeneration",
			"specReplicas", "configSha256",
		], "replica provenance.deployment");
		if (deployment.apiVersion !== "apps/v1") {
			throw new Error("replica provenance Kubernetes apiVersion must be apps/v1");
		}
		const namespace = boundedLabel(deployment.namespace, "replica provenance namespace");
		const specReplicas = requireInteger(
			deployment.specReplicas,
			"replica provenance specReplicas",
			1,
		);
		const generation = requireInteger(deployment.generation, "replica provenance generation", 1);
		const observedGeneration = requireInteger(
			deployment.observedGeneration,
			"replica provenance observedGeneration",
			1,
		);
		if (
			namespace !== scope.namespace
			|| specReplicas !== scope.expectedReplicas
			|| observedGeneration !== generation
		) throw new Error("replica provenance does not match the collected deployment scope");
		return {
			schemaVersion: 1,
			kind: "kubernetes-deployment",
			deployment: {
				apiVersion: "apps/v1",
				name: boundedLabel(deployment.name, "replica provenance deployment name"),
				namespace,
				uidSha256: requireProvenanceSha256(
					deployment.uidSha256,
					"replica provenance UID digest",
				),
				generation,
				observedGeneration,
				specReplicas,
				configSha256: requireProvenanceSha256(
					deployment.configSha256,
					"replica provenance config digest",
				),
			},
		};
	}
	if (provenance.kind === "self-hosted-config") {
		requireExactKeys(
			deployment,
			["replicas", "configSha256", "operatorArtifactSha256"],
			"replica provenance.deployment",
		);
		const replicas = requireInteger(deployment.replicas, "replica provenance replicas", 1);
		if (replicas !== scope.expectedReplicas) {
			throw new Error("replica provenance does not match expectedReplicas");
		}
		return {
			schemaVersion: 1,
			kind: "self-hosted-config",
			deployment: {
				replicas,
				configSha256: requireProvenanceSha256(
					deployment.configSha256,
					"replica provenance config digest",
				),
				operatorArtifactSha256: requireProvenanceSha256(
					deployment.operatorArtifactSha256,
					"replica provenance operator artifact digest",
				),
			},
		};
	}
	requireString(provenance, "kind", "replica provenance");
	throw new Error("replica provenance kind is unsupported");
}
