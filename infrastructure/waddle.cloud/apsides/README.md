# Apsides trial: the `waddle` namespace

An experiment, not a deployment path. This directory ports the `waddle`
namespace from the Flux bundle in [`../gitops`](../gitops) to an
[Apsides](https://github.com/rawkode/apsides) program so we can see what
compiles, what is lost and what Apsides would need before it could replace
Flux for Waddle. Flux remains the only thing that deploys production.

## What is ported

| Flux source (`../gitops/waddle-server`) | Apsides declaration |
| --- | --- |
| `postgresql-cluster.yaml`, `postgresql-monitoring-ingress.yaml` | [`postgres.tsx`](postgres.tsx) |
| `spicedb-*.yaml` | [`spicedb.tsx`](spicedb.tsx) |
| `runtime-`, `openrouter-`, `livekit-external-secret.yaml` | [`runtime-secrets.tsx`](runtime-secrets.tsx) |
| `xmpp-certificate.yaml`, `httproute*.yaml`, `reference-grant.yaml` | [`ingress.tsx`](ingress.tsx) |
| `helmrepository.yaml`, `helmrelease.yaml` (chart rendered to objects) | [`waddle-server.tsx`](waddle-server.tsx) |

The program is one Apsides domain (`<Controller id="waddle">`) because
Apsides binds each controller to exactly one workload namespace.

`waddle-server` is expressed as plain objects rather than a `HelmRelease`:
Apsides reacts to Secret and ConfigMap changes only for Deployments it
manages itself, and that reaction replaces the Flux-era
`WADDLE_RUNTIME_SECRETS_CHECKSUM` / `extraSecretChecksum` /
`reconcile.fluxcd.io/watch` plumbing with `rolloutOn`.

## Generate and compile

Apsides has no released CLI yet. Build it from a checkout that carries the
HTTPRoute CRD import fix (G1 below), then:

```sh
APS=/path/to/apsides/target/debug/aps ./generate.sh   # CRD types + source lock
$APS compile --source program.tsx --output "$(mktemp -d)/out"
APS=/path/to/apsides/target/debug/aps ./gaps/check.sh  # every gap probe must still fail
```

Generated CRD types and the embedded SDK live in the ignored `.aps/`
directory. `generate.sh` pins each CRD to the operator version the Flux
bundle installs.

## Gaps

`program.tsx` compiles only after the degradations marked `GAP-*` in the
source. Each gap has a probe under [`gaps/`](gaps) that declares the Flux
shape and must fail to compile; `gaps/check.sh` fails when a probe starts
compiling, which is the signal to fold that capability back into the program.

The full assessment, including deployment and operations blockers that no
compile probe can show, is in [ASSESSMENT.md](ASSESSMENT.md).
