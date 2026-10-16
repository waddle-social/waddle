// GAP-SERVICE-ACCOUNT: the chart's ServiceAccount. Core-group kinds other than
// ConfigMap, Service and Deployment have no path, not even as a custom resource.
// Rejected with: "namespaced resource descriptor.apiVersion must contain a custom resource group and version".
import { defineNamespacedResource } from "@apsides/kubernetes";
import { namespace, Probe } from "./_base.tsx";

const ServiceAccount = defineNamespacedResource<Record<string, unknown>>({
  apiVersion: "v1",
  kind: "ServiceAccount",
  apiResource: { plural: "serviceaccounts", scope: "Namespaced", schemaSha256: `sha256:${"0".repeat(64)}` },
});

export default <Probe><ServiceAccount metadata={{ name: "waddle-server", namespace }} spec={{}} /></Probe>;
