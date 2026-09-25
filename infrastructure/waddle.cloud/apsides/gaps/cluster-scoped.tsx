// GAP-CLUSTER-SCOPED: Namespace, ClusterIssuer, ClusterSecretStore,
// GatewayClass and StorageClass in ../../gitops are cluster-scoped.
// Only `scope: "Namespaced"` descriptors exist.
// Rejected with: "namespaced resource descriptor.apiResource.scope must be one of Namespaced".
import { defineNamespacedResource, type NamespacedResourceDescriptor } from "@apsides/kubernetes";
import { namespace, Probe } from "./_base.tsx";

const ClusterIssuer = defineNamespacedResource<Record<string, unknown>>({
  apiVersion: "cert-manager.io/v1",
  kind: "ClusterIssuer",
  apiResource: { plural: "clusterissuers", scope: "Cluster", schemaSha256: `sha256:${"0".repeat(64)}` },
} as unknown as NamespacedResourceDescriptor);

export default (
  <Probe>
    <ClusterIssuer metadata={{ name: "letsencrypt-production", namespace }} spec={{ acme: {} }} />
  </Probe>
);
