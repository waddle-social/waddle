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

Apsides has no released CLI yet. Build `aps` (`cuenv task build`) from
`rawkode/apsides` branch `claude/apsides-waddles-gitops-0896zl`; `main` at
`288fcc5` cannot import the HTTPRoute CRD (G1 in
[ASSESSMENT.md](ASSESSMENT.md#fixes-made-to-apsides-in-this-trial)). The tools
also need Deno 2.9 and Python 3 with PyYAML. Then:

```sh
export APSIDES=/path/to/apsides APS=/path/to/apsides/target/debug/aps
./generate.sh                                          # SDK, CRD types, source lock
"$APS" compile --source program.tsx --output "$(mktemp -d)/out"
./gaps/check.sh                                        # each probe still fails for its recorded reason
```

Generated CRD types and the embedded SDK live in the ignored `.aps/`
directory. `generate.sh` pins each CRD to the operator version the Flux
bundle installs.

## Gaps

`program.tsx` compiles only after the degradations marked `GAP-*` in the
source. Each gap has a probe under [`gaps/`](gaps) that declares the Flux
shape and must be rejected with the message on its `// Rejected with:` line.
`gaps/check.sh` fails when a probe starts compiling, which is the signal to
fold that capability back into the program, or when it is rejected for any
other reason.

The full assessment, including deployment and operations blockers that no
compile probe can show, is in [ASSESSMENT.md](ASSESSMENT.md).
