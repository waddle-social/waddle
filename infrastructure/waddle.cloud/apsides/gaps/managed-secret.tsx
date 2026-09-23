// GAP-MANAGED-SECRET: Secrets are observation-only. Waddle's Secrets come from
// ExternalSecrets, so this matters only for the chart's own Secret, whose
// non-sensitive value the program moves into the ConfigMap.
// Rejected with: "namespaced resource descriptor.apiVersion must contain a custom resource group and version".
import { defineNamespacedResource } from "@apsides/kubernetes";
import { namespace, Probe } from "./_base.tsx";

const Secret = defineNamespacedResource<Record<string, unknown>>({
  apiVersion: "v1",
  kind: "Secret",
  apiResource: { plural: "secrets", scope: "Namespaced", schemaSha256: `sha256:${"0".repeat(64)}` },
});

export default <Probe><Secret metadata={{ name: "waddle-server-secrets", namespace }} spec={{}} /></Probe>;
