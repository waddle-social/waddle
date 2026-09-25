// Values shared across the waddle domain. They mirror the Flux bundle in
// ../gitops and must change together with it while both exist.
export const namespace = "waddle";
export const storageClass = "openebs-mayastor";
export const secretStore = { kind: "ClusterSecretStore", name: "onepassword" } as const;
export const gateway = { name: "waddle-gateway", namespace: "default" } as const;
export const xmppHost = "xmpp.waddle.social";
