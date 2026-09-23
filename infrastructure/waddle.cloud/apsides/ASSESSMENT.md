# Can Apsides run Waddle's GitOps?

Assessed against Apsides `main` at `288fcc5` (September 2026) and the Flux
bundle in [`../gitops`](../gitops). Waddle runs Kubernetes v1.35.0 on Talos
v1.12 and deploys from GitHub Actions on Namespace runners.

## Verdict

**No, not today, and not all of it even once Apsides matures.**

- **Deployment is blocked outright.** Apsides has no path from a compiled
  program to Waddle's cluster.
- **The `waddle` namespace is portable in shape.** This directory compiles
  it, but only after removing things `waddle-server` needs to run safely.
- **The platform layer is out of scope for Apsides by design.** That layer
  is ten Helm-installed operators and the cluster-scoped objects around them.

The realistic target is a hybrid. Flux keeps the platform layer and Apsides
owns application namespaces, starting with `waddle`. That requires the
Apsides work in the priority list at the end.

## What the port shows

`program.tsx` compiles to one domain with:

- 19 managed resources, 5 inputs and 5 rollout reactions;
- 75 generated reactor cases;
- a reviewed permission set: 66 namespaced RBAC rules, all name-scoped
  except the 11 `create` rules, and no cluster-scoped access.

Where it is better than the Flux bundle:

- **Rollout on Secret change is native.** `rolloutOn` restarts
  `waddle-server` when any of these change:
  - its ConfigMap;
  - `waddle-runtime-secrets`, `waddle-clustering-keypool` or
    `waddle-livekit-config`;
  - CNPG's `postgresql-app`.

  It uses an HMAC revision and never reads the values into the program. That
  replaces three pieces of Flux plumbing: the `WADDLE_RUNTIME_SECRETS_CHECKSUM`
  template key, `valuesFrom` into `extraSecretChecksum`, and the
  `reconcile.fluxcd.io/watch` label. Today the chart hashes the extra
  Secrets with Helm `lookup`, so a rotation reaches the Pod template only
  when Flux next renders the release. That is immediate for the two Secrets
  labelled for Flux's watch, and otherwise waits for the 30-minute interval.
  `postgresql-app` is not hashed at all, so a CNPG credential rotation never
  triggers a rollout.
- **Custom resources are typed.** CNPG, ExternalSecret, Certificate, HTTPRoute,
  ReferenceGrant and SpiceDBCluster types are generated from the CRD versions
  the bundle installs. Typos fail typecheck instead of failing
  `flux reconcile`.
- **Risks are explicit.** The compiled plan lists them, each needing platform
  approval before deploy:
  - `mutable-workload-image`: the `:main` tag;
  - `opaque-custom-resource-effects`: CNPG and SpiceDB create workloads
    Apsides cannot see;
  - Secret-read authority.
- **Missing inputs block mutations.** If a Secret is missing, the Deployment
  is not mutated, so the chart's `optional: false` references are no longer
  needed.
- **PodDisruptionBudget and NetworkPolicy work.** They go through the
  custom-resource path (see [`builtin-resources.ts`](builtin-resources.ts)),
  untyped and with an OpenAPI digest instead of a CRD digest.

## Blockers

### Deploy: nothing reaches the cluster

| # | Blocker | Evidence |
| --- | --- | --- |
| D1 | `aps deploy` refuses any API server except `v1.34.3`; Waddle runs `v1.35.0`. This is a deliberate qualification gate, not a bug. | `crates/cli/src/commands/deploy/cluster.rs:87`, `crates/platform/src/preview_authority.rs:118` |
| D2 | Deploy-time API discovery listed every group at `{group}/v1`. It would reject ExternalSecret (`v1beta1`), ReferenceGrant (`v1beta1`) and SpiceDBCluster (`v1alpha1`). | `cluster.rs:95-119`. Fixed on the Apsides branch (G2). |
| D3 | Deployment state (inventory, claims, reviews, audit) lives in a local SQLite file. It "must not" be on an ephemeral CI filesystem and there is no remote backend. Namespace runners are ephemeral. Losing the file after the first deploy leaves the next upgrade in "recovery required", and no recovery procedure exists. | `docs/guides/build-and-deploy.md`, `docs/reference/INVENTORY.md`, `STATUS.md` |
| D4 | There is no tooling to provision the platform records `aps deploy` requires. Only the acceptance test harness writes them, for kind clusters. The records are: the namespace registry and anchor, trust policy, permission ceiling, deployer identity, proxy qualification and rollout key. | `crates/acceptance/src/bootstrap.rs`, `docs/reference/PLATFORM-CONTRACT.md` |
| D5 | There is no release-signing workflow; `aps release` writes unsigned provenance. OpenSSH signing is verifiable today. The GitHub-attestation path passes `--deny-self-hosted-runners`, and Namespace runners count as self-hosted. | `crates/release/src/provenance.rs`, `STATUS.md` |
| D6 | The executor identity is `github-<run id>-<attempt>`. Re-running a failed deploy job changes the attempt number and blocks resume, because executor takeover is unsupported. | `crates/cli/src/commands/deploy.rs:146-160` |
| D7 | Apsides states it is not ready for production use. | `README.md`, `STATUS.md` |

### Workload: `waddle-server` would run degraded

Each gap has a probe in [`gaps/`](gaps) that `gaps/check.sh` requires to fail.

| Gap | Waddle impact |
| --- | --- |
| `GAP-PROBES` | No liveness or readiness probes and no preStop sleep. Rollouts route traffic to Pods before `/ready` and cut connections without the ADR-0017 drain, so zero-downtime rollouts are lost. |
| `GAP-POD-SPEC` | No `terminationGracePeriodSeconds`: the default 30 s is below the chart's 45 s drain budget. Also no `fsGroup` and no anti-affinity. |
| `GAP-HEADLESS-SERVICE` | No headless Service with `publishNotReadyAddresses`. Clustering bootstrap needs a DNS name that resolves to every Pod; a ClusterIP resolves to one load-balanced address, so peers can dial themselves and cluster formation becomes unreliable. |
| `GAP-DOWNWARD-API` | No `fieldRef`, so no `WADDLE_CLUSTERING_POD_TEMPLATE_HASH`. The rollout-aware claim backoff is disabled. |
| `GAP-ROLLOUT-STRATEGY` | No `maxSurge`/`maxUnavailable` and no `Recreate`. With two replicas the defaults match today's values, but the one-shot Recreate cutovers the HelmRelease records have no expression. |
| `GAP-VOLUMES` | No volumes. The ai-chatbot extension's OpenRouter key file cannot be mounted. The data emptyDir and the XMPP TLS mount are also lost; the server never reads `WADDLE_XMPP_TLS_*`, so losing the TLS mount does nothing today. |
| `GAP-CONTAINER-SECURITY` | The SDK rejects every `securityContext`, although the compiler accepts `runAsNonRoot` and `allowPrivilegeEscalation: false`. Waddle loses drop-ALL capabilities, `runAsUser`/`runAsGroup` 1000 and non-root enforcement. |
| `GAP-SERVICE-ACCOUNT` | No ServiceAccount: core-group kinds other than ConfigMap, Service and Deployment have no path. The program falls back to the default account with token automount off. |
| `GAP-MANAGED-SECRET` | Secrets are observe-only. This is harmless here: the chart's Secret held only non-secret values, which moved to the ConfigMap. |

### Coverage: what Apsides cannot own at all

| Gap | Waddle impact |
| --- | --- |
| `GAP-CROSS-NAMESPACE` | One controller manages exactly one namespace. The bundle spans 13 namespaces, so full coverage means 13 domains, releases and controller generations. |
| `GAP-CLUSTER-SCOPED` | Cluster-scoped objects are impossible: 11 Namespaces, plus `ClusterIssuer`, `ClusterSecretStore`, `GatewayClass`, `StorageClass` and every CRD. |
| No Helm | Ten operators arrive as `HelmRelease`s: cert-manager, CNPG, external-dns, ESO, Alloy, OpenEBS, LiveKit, 1Password Connect, Teleport, and waddle-server itself. Their charts render CRDs, ClusterRoles, webhooks, DaemonSets and StatefulSets. Declaring the `HelmRelease` objects as custom resources would work, but Flux would still be doing the deploying. |
| No ordering | Flux `dependsOn` and health checks order CRDs before custom resources and operators before their objects. Apsides has no readiness predicates or cross-resource gating; the spec marks them "Later". |

### Delivery model

Flux takes a new server image as a values edit. The server pipeline rewrites
the digest in `helmrelease.yaml` and pushes an OCI artifact.

In Apsides, desired state is compiled into the controller binary. Every
server release, extension digest bump or config change would require:

1. compile the program;
2. build a native Rust controller image with a pinned toolchain, BuildKit
   and a vendored Cargo cache;
3. sign and push that image;
4. run a controller-generation transition, which rotates RBAC and hands off
   the writer.

That is a heavy loop for a service that ships on every merge to `main`.
Apsides has no mechanism for a parameter that changes on every release, such
as an image digest, without a new generation.

### Authoring friction found while porting

- **CRD import crashed.** `aps crds fetch` crashed on the Gateway API
  HTTPRoute CRD. Fixed on the Apsides branch (G1).
- **Derived ids overflow.** The derived rollout id
  `rollout-<input id>-<deployment id>` exceeds the SDK's own 63-character
  limit with Waddle's names (`waddle-server-config`,
  `waddle-clustering-keypool`). The program sets short ids as a workaround.
  Fix on the Apsides branch (G3).
- **No diagnostics.** `aps compile` discards every compiler diagnostic, by
  design, to avoid echoing private source. Authors only see "source
  compilation failed". The reasons recorded in the gap probes came from
  calling the compiler library directly.
- **No SDK restore.** A committed project cannot restore its ignored
  `.aps/sdk` from a fresh clone, because `aps init` refuses an existing
  project. `generate.sh` works around this.
- **No file reads.** Programs cannot read files at build time, so the 8 KB
  of CNPG monitoring queries are generated into a module from the Flux
  ConfigMap.
- **SDK narrower than compiler.** The SDK accepts less than the compiler:
  `command`, `args` and `securityContext` are valid IR but rejected by the
  SDK.

## Fixes made to Apsides in this trial

On branch `claude/apsides-waddles-gitops-0896zl` of `rawkode/apsides`:

- **G1:** bounded CRD arrays generate plain arrays, not tuple unions, so
  HTTPRoute imports.
- **G2:** deploy-time API discovery checks each resource at the group-version
  its IR declares.
- **G3:** derived ids that would exceed 63 characters become a readable
  prefix plus a hash; ids that already fit are unchanged.

## What Apsides needs, in order, for the `waddle` namespace

1. **Kubernetes version qualification.** Qualify v1.35, and define a version
   policy that does not break on every cluster minor upgrade (D1).
2. **Durable deployment state.** Either a supported remote or in-cluster
   store, or a documented recovery procedure (D3).
3. **Pod template coverage.** Volumes (Secret, ConfigMap and emptyDir),
   probes, lifecycle hooks, `terminationGracePeriodSeconds`, rollout
   strategy, downward-API `fieldRef`, and the restrictive `securityContext`
   set in the SDK.
4. **Service coverage.** Headless Services and `publishNotReadyAddresses`,
   plus ServiceAccount as a managed kind.
5. **Platform bootstrap and signing.** An `aps` command, or a documented
   procedure, that provisions the platform records for a real cluster, and a
   signing workflow (D4, D5).
6. **Resumable CI deploys.** A stable executor identity across job re-runs
   (D6).
7. **Cheap image rollout.** A way to roll a new image digest without a full
   controller generation, or a measured generation transition cheap enough
   to run on every merge.
8. **Readiness gating.** Health or readiness gating between managed
   resources.

Items 1–5 are blocking. Items 6–8 decide whether Apsides would be pleasant
to run once deployment works at all.
