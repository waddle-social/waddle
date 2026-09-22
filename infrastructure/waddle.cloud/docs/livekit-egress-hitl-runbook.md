# LiveKit Egress — HITL deploy & verify runbook (#1023)

> **HITL (human-in-the-loop).** This component rides the 1Password
> `ExternalSecret` sync path that has stalled LiveKit rollouts before
> (`SecretSyncedError`, and the 2026-06-17 `use_external_ip` STUN-egress
> outage). It MUST be deployed and verified against the live cluster by a
> human — never an AFK merge. The PR ships the declarative manifests, the
> Go invariant tests, and the chart-publish wiring; the steps below are the
> live work that closes acceptance criteria 1–3.

## What this provisions

LiveKit composite/track egress records rooms and uploads artifacts to the
existing Cloudflare R2 bucket. Egress does **not** talk to the SFU directly —
it coordinates over a shared **Redis** message bus, which the cluster did not
previously run. So this change provisions three things:

1. **`livekit-redis`** — a minimal in-cluster Redis (message bus only, no
   persistence) in the `livekit` namespace. Now the foundational component of
   the LiveKit stack and the owner of the `livekit` namespace.
2. **`livekit-sfu`** reconfigured to use that Redis (`redis.address` in the
   LiveKit config) so it can dispatch egress jobs. Config-only change via the
   HelmRelease values — the SFU chart itself is unchanged.
3. **`livekit-egress`** — the egress Deployment, pointed at the SFU
   (`ws_url`), the Redis bus, and R2 storage.

Dependency / Flux `dependsOn` order: `namespace → redis → sfu → egress`.

## 1Password prerequisites (operator)

Add to the **`livekit-sfu`** 1Password item (same item the SFU keys live in):

| property        | meaning                                                        |
|-----------------|----------------------------------------------------------------|
| `egress-key`    | API key NAME the egress authenticates to the SFU with          |
| `egress-secret` | its HMAC secret — must be added to the SFU `keys.yaml` too      |

R2 credentials are **reused** from the existing `server-runtime-production`
item (`r2-access-key-id` / `r2-secret-access-key`) — the same creds
waddle-server already uses for `waddle-social-files`. No new R2 key is minted.

> The egress key/secret is a dedicated pair (mirroring the `webhook-key`
> precedent) so an egress credential leak cannot forge room-join JWTs or
> webhooks. The SFU `keys.yaml` is wired to consume the same
> `egress-key`/`egress-secret` properties automatically (see
> `gitops/livekit-sfu/external-secret.yaml`), so the SFU accepts the egress
> worker's auth — **the operator only adds the two 1Password properties; no
> manual `keys.yaml` edit is needed.** Once added, BOTH the `livekit-sfu`
> and `livekit-egress` ExternalSecrets must reconcile without
> `SecretSyncedError` (a missing property wedges the whole sync).

## Deploy & verify (closes AC 1–3)

1. **Create the 1Password properties** above; confirm the `livekit-sfu`
   `ExternalSecret` reconciles with **no `SecretSyncedError`**:
   ```
   kubectl -n livekit get externalsecret livekit-sfu-api-keys
   kubectl -n livekit get externalsecret livekit-egress
   ```
2. **Merge → Flux republish.** The egress chart version bump triggers the OCI
   republish (per the chart-publish rule); watch the HelmReleases settle:
   ```
   flux -n livekit get helmreleases
   kubectl -n livekit rollout status deploy/livekit-redis
   kubectl -n livekit rollout status deploy/livekit-egress
   ```
   **AC1:** `livekit-egress` Deployment is `Available`/healthy.
3. **AC2 — egress authenticates to the SFU.** Egress logs should show a
   successful connection to the Redis bus and SFU (no auth-rejected loops):
   ```
   kubectl -n livekit logs deploy/livekit-egress | grep -iE "redis|starting|registered"
   ```
4. **AC3 — write a test artifact to R2.** Start a short room-composite (or
   track) egress against a test room via `lk egress start` (or the SFU egress
   API) and confirm the object lands in `waddle-social-files`:
   ```
   # using the rclone/aws profile for the R2 endpoint
   aws --endpoint-url https://<r2-account>.r2.cloudflarestorage.com \
       s3 ls s3://waddle-social-files/recordings/
   ```
   Delete the test artifact afterward.

## Rollback

`flux suspend hr livekit-egress -n livekit` and revert the SFU `redis.address`
value if egress dispatch destabilizes the SFU. Redis is stateless, so deleting
the `livekit-redis` Deployment is non-destructive.

## Known landmines

- **`SecretSyncedError` on the 1Password path** has blocked *all* livekit
  rollouts before. If the egress release will not progress, check the
  `ExternalSecret` status first — a missing property name fails the whole sync.
- **NetworkPolicy egress.** The SFU's `use_external_ip` STUN-egress omission
  caused a prod outage; do not narrow the existing SFU egress allowances.
  Egress additionally needs egress to the SFU (`7880`), Redis (`6379`), and R2
  (`443`).
