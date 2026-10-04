import { ConfigMap } from "@apsides/kubernetes";
import { Cluster } from "./.aps/crds/cnpg-cluster.ts";
import { monitoringQueries } from "./.aps/generated/monitoring-queries.ts";
import { namespace, storageClass } from "./site.ts";

// The queries are extracted from ../gitops/waddle-server/postgresql-monitoring-ingress.yaml
// by generate.sh: an Apsides program cannot read files at build time.
export const WaddleDatabase = () => (
  <>
    <ConfigMap
      metadata={{ name: "postgresql-monitoring-ingress", namespace, labels: { "cnpg.io/reload": "" } }}
      data={{ queries: monitoringQueries }}
    />
    <Cluster
      metadata={{ name: "postgresql", namespace }}
      deletionPolicy="orphan"
      spec={{
        instances: 2,
        bootstrap: { initdb: { database: "waddle", owner: "waddle" } },
        monitoring: {
          customQueriesConfigMap: [{ name: "postgresql-monitoring-ingress", key: "queries" }],
          disableDefaultQueries: false,
        },
        storage: { size: "10Gi", storageClass },
        walStorage: { size: "2Gi", storageClass },
        resources: {
          requests: { memory: "512Mi", cpu: "250m" },
          limits: { memory: "1Gi" },
        },
      }}
    />
  </>
);
