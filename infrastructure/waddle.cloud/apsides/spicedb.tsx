import { Cluster } from "./.aps/crds/cnpg-cluster.ts";
import { ExternalSecret } from "./.aps/crds/external-secret.ts";
import { SpiceDBCluster } from "./.aps/crds/spicedb-cluster.ts";
import { namespace, secretStore, storageClass } from "./site.ts";

const spicedbCredentials = [
  { secretKey: "postgres_username", remoteRef: { key: "spicedb", property: "postgres-username" } },
  { secretKey: "postgres_password", remoteRef: { key: "spicedb", property: "postgres-password" } },
];

export const SpiceDB = () => (
  <>
    <ExternalSecret
      metadata={{ name: "spicedb-postgres-app-user", namespace }}
      spec={{
        refreshInterval: "1h",
        secretStoreRef: secretStore,
        target: {
          name: "spicedb-postgres-app-user",
          creationPolicy: "Owner",
          template: {
            engineVersion: "v2",
            type: "kubernetes.io/basic-auth",
            data: { username: "{{ .postgres_username }}", password: "{{ .postgres_password }}" },
          },
        },
        data: spicedbCredentials,
      }}
    />
    <ExternalSecret
      metadata={{ name: "spicedb-config", namespace }}
      spec={{
        refreshInterval: "1h",
        secretStoreRef: secretStore,
        target: {
          name: "spicedb-config",
          creationPolicy: "Owner",
          template: {
            engineVersion: "v2",
            type: "Opaque",
            data: {
              datastore_uri:
                "postgresql://{{ .postgres_username | urlquery }}:{{ .postgres_password | urlquery }}@postgresql-spicedb-rw:5432/spicedb?sslmode=disable",
              preshared_key: "{{ .preshared_key }}",
            },
          },
        },
        data: [
          ...spicedbCredentials,
          { secretKey: "preshared_key", remoteRef: { key: "server-runtime-production", property: "spicedb-preshared-key" } },
        ],
      }}
    />
    <Cluster
      metadata={{ name: "postgresql-spicedb", namespace }}
      deletionPolicy="orphan"
      spec={{
        instances: 1,
        bootstrap: {
          initdb: { database: "spicedb", owner: "spicedb", secret: { name: "spicedb-postgres-app-user" } },
        },
        storage: { size: "10Gi", storageClass },
      }}
    />
    <SpiceDBCluster
      metadata={{ name: "spicedb", namespace }}
      deletionPolicy="orphan"
      spec={{
        channel: "stable",
        secretName: "spicedb-config",
        config: { datastoreEngine: "postgres", replicas: 1 },
      }}
    />
  </>
);
