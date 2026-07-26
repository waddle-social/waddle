# Self-hosted LiveKit multi-tenancy patterns for Waddle

Research date: 2026-07-26. Research only — no code changes. Feeds a locked
roadmap decision on media-layer (LiveKit SFU) tenancy, not the whole-platform
multi-tenancy effort tracked separately (see wayfinder map issue #1489, which
explicitly excludes XMPP-domain/storage/auth multi-tenancy from this scope).

## Executive summary and recommendation

For a **self-hosted-only, EU-data-residency-bound** deployment, none of the
three patterns is complete on its own — LiveKit's server-native primitives for
tenant isolation are thin (one API-key list, one webhook signer, no
room/participant quota or bandwidth cap keyed by tenant, no per-tenant TURN
secret in the embedded server). The realistic choices are:

- **(b) room-name namespacing with a single API key/secret pair** is what
  Waddle already has today (one `LIVEKIT_API_KEY`/`LIVEKIT_API_SECRET`, one
  webhook secret, `CallId`-shaped room names). It is the cheapest to keep
  running and is fully self-host-compatible, but every tenant currently trusts
  the same signing key and shares one physical SFU cluster with only
  Kubernetes pod resource limits as blast-radius control — no LiveKit-native
  per-tenant quota, no cryptographic separation between tenants' tokens.
- **(a) multiple API key/secret pairs on one `livekit-server`** is a strict
  superset of (b): `keys:` in `config-sample.yaml` is already a map, and
  Waddle's chart already renders an `apiKeys` map (currently populated with
  one pair). Moving to per-tenant keys costs little operationally (no new
  clusters, no new infra bill) and materially improves the "hard to reverse"
  properties: distinct tenant JWTs, distinct webhook-signing secrets (webhook
  config still only takes **one** `api_key`, see Pattern A section below —
  this is a real gap that must be designed around), and a natural point to
  attach a tenant claim if the JWT `iss`/room-naming convention is chosen
  carefully now.
- **(c) per-tenant SFU deployments** gives the strongest isolation (network,
  noisy-neighbor, blast radius, per-tenant capacity/cost accounting) but is
  the most expensive pattern to run self-hosted (N Kubernetes deployments, N
  TURN endpoints, N Redis instances for clustering, N sets of node-selector
  tuning) and LiveKit gives no built-in fleet/tenant router — Waddle would
  have to build the "which cluster does this tenant's call go to" layer
  itself.

**Recommendation: start with (b)+(a) hybrid now, architect so (c) is a later
option for specific tenants, not a rewrite.**

Concretely:
1. Move from Waddle's current single API key/secret pair to **one API
   key/secret pair per tenant** (pattern a) on the existing single
   `livekit-server` deployment. This is a low-cost, high-value change: it
   makes tenant JWTs cryptographically distinct (a leaked token or key can
   only mint grants for its tenant's rooms) and gives every downstream
   verifier (webhook receiver, `mint_join_token`) a **key identity that is
   already, functionally, a tenant identity** — because LiveKit's own webhook
   verifier resolves the signing key by the JWT `iss` claim
   (`github.com/livekit/protocol/webhook/verifier.go`: `v.APIKey()` →
   `provider.GetSecret(v.APIKey())`), a per-tenant key becomes a free tenant
   discriminator on every webhook, with no new field needed on the LiveKit
   side.
2. Continue namespacing rooms with a tenant-scoped `CallId` prefix/shape
   (pattern b) as defense in depth, not as the sole isolation mechanism — it
   is cheap and already baked into Waddle's data model, but must never be the
   *only* thing standing between tenants (a JWT is a bearer credential scoped
   only by the `room` claim, not by "which tenant issued this room name").
3. Treat (c), per-tenant SFU clusters, as an **exit ramp reserved for
   individual large/regulated tenants** (e.g. a customer with a stricter EU
   sub-region or data-processing-agreement requirement than the shared
   cluster can offer), not the default. Design the tenant→key mapping (step 1)
   so that routing a specific tenant to a dedicated `livekit-server` cluster
   later is a config/routing change (new URL + key pair for that tenant) and
   not a rewrite of token minting, webhook verification, or egress code.
4. EU residency is **not** a LiveKit server concern at all — it is answered
   entirely by *where Waddle deploys* the `livekit-server`/TURN/egress pods
   and *which S3-compatible endpoint/region* egress uploads to (LiveKit's
   `S3Upload` proto takes an arbitrary `region`/`endpoint`, so any
   EU-region/EU-sovereign S3-compatible object store works, self-hosted or
   not). No pattern (a/b/c) changes this; residency is orthogonal to the
   tenancy pattern.

---

## Pattern (a): multiple API key/secret pairs on one `livekit-server`

### Token / VideoGrant scoping
- `keys:` in the canonical config is a literal map of `key: secret` pairs,
  confirmed from the primary config reference:
  `keys:\n  key1: secret1\n  key2: secret2` — comment: "API key / secret
  pairs. Keys are used for JWT authentication, server APIs would require a
  keypair in order to generate access tokens and make calls to the server."
  (Source: `config-sample.yaml`,
  https://raw.githubusercontent.com/livekit/livekit/master/config-sample.yaml)
- A LiveKit access token is a JWT whose `iss` claim identifies which key
  signed it, and whose `video` claim carries a `VideoGrant`. Per the Tokens &
  grants docs (https://docs.livekit.io/frontends/authentication/tokens/),
  the `VideoGrant` fields are:
  - `room` — "Name of the room, required if join or admin is set"
  - `roomJoin` — permits entry into that room
  - `canPublish`, `canSubscribe`, `canPublishData` — media/data permissions
  - `canPublishSources` — restricts publish to specific `TrackSource`s
    (camera, microphone, screen_share, screen_share_audio) when `canPublish`
    is set
  - `canUpdateOwnMetadata` — self-metadata edit permission
  - `roomCreate` — "Permission to create or delete rooms"
  - `roomList` — "Permission to list available rooms"
  - `roomAdmin` — "Permission to moderate a room"
  - `roomRecord` — "Permission to use Egress service"
  - `ingressAdmin` — "Permission to use Ingress service"
  - `hidden` — hide participant from others in the room
  - `destinationRoom` — name of a room a participant can be forwarded to
  - `kind` — participant type (standard/ingress/egress/sip/agent/connector)
  (Cross-checked against the Go SDK's `VideoGrant` struct via
  https://pkg.go.dev/github.com/livekit/protocol/auth and the JS SDK
  reference https://docs.livekit.io/reference/server-sdk-js/interfaces/VideoGrant.html,
  which additionally show `canSubscribeMetrics` (Go) / `canManageAgentSession`
  (Python) — not present in Waddle's usage.)
- Because each key has its own secret, a per-tenant key pair means a token
  minted for tenant X's room can never be replayed/verified against tenant
  Y's secret and vice versa — this is a real cryptographic tenant boundary
  that pure room-name namespacing (pattern b) does not provide by itself.
  **However**, LiveKit itself does not put a tenant identifier in the
  `VideoGrant` or any first-class "tenant" claim — the key *is* the tenant
  boundary; nothing enforces that key K's grants only ever reference
  tenant-K-prefixed room names except application code at mint time. Waddle
  would still own the room-naming/key-pairing invariant.
- Waddle's current `mint_join_token` (`server/crates/waddle-sfu/src/token.rs`)
  already builds exactly this shape of grant (`room_join`, `room`,
  `can_publish`, `can_subscribe`, `can_publish_data`) for a single room —
  moving to per-tenant keys requires no VideoGrant redesign, only passing a
  tenant-selected `ApiKey`/`ApiSecret` pair into the existing mint function
  instead of the current process-wide singleton loaded once in
  `SfuConfig::from_env` (`server/crates/waddle-sfu/src/config.rs`).

### Webhook attribution
- LiveKit signs webhooks as a JWT in the `Authorization` header containing a
  sha256 hash of the payload (https://docs.livekit.io/home/server/webhooks/).
- **Primary-source confirmation from the verifier implementation itself**
  (`github.com/livekit/protocol`, file `webhook/verifier.go`,
  https://raw.githubusercontent.com/livekit/protocol/main/webhook/verifier.go):
  ```go
  authToken := r.Header.Get(authHeader)
  v, err := auth.ParseAPIToken(authToken)      // parses iss claim -> APIKey()
  secret := provider.GetSecret(v.APIKey())      // look up secret by that key
  _, claims, err := v.Verify(secret)
  ```
  This proves two things primary-source-grounded:
  1. **Multiple keys are natively supported for webhook verification** — the
     receiver is expected to hold a `KeyProvider` capable of resolving *any*
     configured key's secret by the `iss`/API-key value carried in the
     token, not just one hardcoded secret. A multi-tenant receiver can use
     the resolved API key as the tenant discriminator for free.
  2. The **chart-level config gap is real**: `config-sample.yaml`'s
     `webhook:` block only accepts **one** `api_key` to sign outgoing
     webhooks (`webhook.api_key: <api_key>` — "the API key to use in order to
     sign the message; this must match one of the keys LiveKit is configured
     with"). LiveKit's webhook *notifier* (server side, sending) signs with
     exactly one configured key at a time; it does not fan out one webhook
     event per tenant key. So on self-hosted LiveKit, "multiple keys" gives
     you multiple valid *verifier* secrets but does **not** give you
     automatic per-tenant webhook signing unless Waddle runs one webhook
     config per tenant key (which the single `livekit-server` config schema
     does not support — it is a singleton `webhook:` block) — or unless
     Waddle keeps signing all webhooks with one dedicated "webhooks key"
     separate from the per-tenant join-token keys, and attributes tenancy via
     `event.room.name`/`event.room.metadata` instead (this is the practical
     path).
- Regardless of key scheme, LiveKit's `WebhookEvent` envelope
  (https://docs.livekit.io/home/server/webhooks/) carries only `id`,
  `createdAt`, `event`, and nested `room`/`participant`/`egress`/`ingress`
  objects — **no tenant/org/project field of any kind**. This matches
  Waddle's actual `LiveKitWebhookEvent`/`RoomInfo` types
  (`server/crates/waddle-sfu/src/webhook.rs`): `RoomInfo.name` (Waddle's
  `CallId`) is the only room-identifying field available, confirmed against
  the vendor's own doc which does not enumerate additional per-room tenant
  fields either. **Tenant attribution from a webhook, under any of the three
  patterns, always ultimately falls back to parsing the room name (or room
  metadata) string** — LiveKit gives no structured tenant field to lean on
  instead.

### Egress isolation
- The `S3Upload` protobuf message (canonical source:
  `github.com/livekit/protocol`, `protobufs/livekit_egress.proto`,
  https://github.com/livekit/protocol/blob/main/protobufs/livekit_egress.proto)
  defines: `access_key`, `secret`, `session_token`, `assume_role_arn` (+
  `assume_role_external_id`), `region`, `endpoint`, `bucket`,
  `force_path_style`, `metadata` (map<string,string>), `tagging`,
  `content_disposition`, `proxy`. Parallel `GCPUpload` (`credentials`,
  `bucket`, `proxy`) and `AzureBlobUpload` (`account_name`, `account_key`,
  `container_name`) messages exist for the other two cloud backends.
- Because `bucket`, `access_key`/`secret` (or `assume_role_arn` for STS-style
  role assumption), `region`, and `endpoint` are **per-egress-request
  fields**, not just server-config defaults, a single `livekit-server`
  deployment (or single egress fleet) can point different egress jobs at
  different tenant-owned buckets/prefixes/IAM roles/regions **without running
  separate egress infrastructure per tenant** — this is the practical
  IAM-boundary lever available under pattern (a) or (b): mint an
  `assume_role_arn`/STS session scoped to the calling tenant's prefix/bucket
  per egress request rather than relying on one shared static credential with
  bucket-wide access.
- Waddle currently has **no egress config anywhere in the chart** (confirmed:
  no S3/egress block in `infrastructure/waddle.cloud/charts/livekit-sfu/values.yaml`
  or `templates/configmap.yaml`) — this is a from-scratch design surface, not
  a migration, which is good news: the per-request `S3Upload` fields mean the
  IAM-boundary decision (shared bucket+prefix vs per-tenant bucket vs
  per-tenant assumed role) can be made independent of the (a)/(b)/(c)
  API-key/room-naming/cluster choice.

### TURN credential separation
- The embedded TURN server config in `config-sample.yaml` is a single
  `turn:` block (`enabled`, `udp_port`, `tls_port`, `domain`, `ttl_seconds`
  defaulting to 300, `allow_restricted_peer_cidrs`, `deny_peer_cidrs`) with
  **no per-key or per-tenant secret field** — it uses the deployment's
  overall TURN mechanism, matching Waddle's actual `turn.secretName` /
  `LIVEKIT_TURN_SHARED_SECRET` single shared secret.
  (Source: `config-sample.yaml`, `turn:` section, same raw URL as above.)
- Separately, `rtc.turn_servers` in the same file lets the SFU hand clients a
  **list of external TURN servers**, each with its own `host`, `port`,
  `protocol`, and independent `secret`/`secret_file`/`username`+`credential`.
  This is the one place LiveKit's own config format supports multiple,
  independently-secreted TURN endpoints from a single `livekit-server` — but
  it is a static list configured at the SFU level, not something a token or
  API key selects dynamically per tenant. Achieving true per-tenant TURN
  secrets under pattern (a)/(b) would mean running N external coturn
  instances (each with its own `use-auth-secret`, a coturn-native option
  referenced by LiveKit's own TURN deployment guidance for the
  time-limited-credential HMAC scheme) and statically listing all of them in
  `rtc.turn_servers` — LiveKit does not route a session to "its tenant's"
  TURN entry for you.
- Waddle today: one embedded/shared TURN secret for every tenant
  (`server/crates/waddle-sfu/src/config.rs`: single
  `LIVEKIT_TURN_SHARED_SECRET`). Under pattern (a), moving to genuinely
  separate TURN secrets per tenant is possible but requires operating
  multiple TURN endpoints — LiveKit does not turn "multiple API keys" into
  "multiple TURN realms" automatically.

### Noisy-neighbor controls
- `config-sample.yaml`'s `limit:` block (node-level, **not per-key**):
  `num_tracks` (defaults 400/CPU, up to 8000), `bytes_per_sec` (defaults
  1_000_000_000, "just under 10 Gbps"), `subscription_limit_video`,
  `subscription_limit_audio`, `max_metadata_size`, `max_attributes_size`,
  `max_room_name_length`, `max_participant_identity_length`. All apply
  globally to the node, not scoped to a key/tenant.
- `room:` block (also global default, **not per-key**): `auto_create`,
  `empty_timeout` (300s), `departure_timeout` (20s), `max_participants` (0 =
  unlimited by default), `enabled_codecs`, `enable_remote_unmute`,
  `playout_delay{enabled,min,max}`, `sync_streams`. Rooms created explicitly
  via `CreateRoom` API can override these per room (confirmed by the
  config's own comment: "Each room created will inherit these settings. If
  rooms are created explicitly with CreateRoom, they will take precedence
  over defaults") — so `max_participants` **can** be set per room at
  creation time (via the room-create API call, i.e. an application-layer
  choice per tenant/room), even though there is no per-*key* limit construct.
  (Source for both blocks: `config-sample.yaml`, `limit:` and `room:`
  sections.)
- There is **no native per-API-key or per-tenant rate limit, bandwidth cap,
  or participant quota** anywhere in the config reference — confirmed
  absent from the full file content fetched above. The only global levers
  are node-wide (`limit:`) or per-room-at-creation-time (`room.max_participants`
  etc., settable via the CreateRoom API per call).
- **Conclusion**: under (a) and (b), noisy-neighbor protection beyond
  Kubernetes pod CPU/memory limits (Waddle's current sole protection, per the
  chart's `resources: {cpu: 500m/2, memory: 512Mi/2Gi}`) requires
  **application-layer enforcement** — e.g. Waddle's own code calling
  `max_participants` on `CreateRoom` per tenant/room, or its own
  admission/rate-limiting logic in front of token minting — LiveKit gives no
  native "tenant X gets Y bandwidth" dial. Only pattern (c) (separate
  clusters, separate node pools) turns this into infrastructure-level
  (not merely config-level) isolation, because a node's `limit:`/resource
  ceiling is then dedicated to one tenant's traffic by construction.

### Per-tenant usage metering
- Natively available signals, all confirmed primary-source:
  - **Webhooks**: `room_started`, `room_finished`, `participant_joined`,
    `participant_left`, `participant_connection_aborted`,
    `track_published`, `track_unpublished`, `egress_started`,
    `egress_updated`, `egress_ended`, `ingress_started`, `ingress_ended`
    (https://docs.livekit.io/home/server/webhooks/). These carry room/
    participant/track state changes but, as noted above, no tenant field —
    any per-tenant usage rollup must be derived by parsing the room name (or
    room metadata) Waddle itself set, which is exactly what pattern (b)'s
    `CallId` convention already provides today.
  - **Prometheus**: `config-sample.yaml` exposes `prometheus_port` for a
    `:6789/metrics` endpoint — global process metrics, not tenant-scoped by
    LiveKit; any tenant breakdown requires either per-tenant clusters
    (pattern c, where "which cluster" becomes the tenant label for free) or
    custom application-side correlation (pattern a/b) of webhook events with
    a tenant map.
  - **Room/participant metadata**: `VideoGrant.canUpdateOwnMetadata` and
    room metadata fields exist, and could carry a tenant tag Waddle sets
    itself, but this is Waddle-authored data, not a LiveKit-native usage
    metering feature.
- **Conclusion**: LiveKit gives raw events and raw process metrics; all
  per-tenant *aggregation* (cost attribution, quota tracking, billing) is a
  build-it-yourself layer regardless of pattern — the only pattern that
  gives free infrastructure-level attribution is (c), because the deployment
  itself is the tenant boundary.

---

## Pattern (b): room-name namespacing with a single API key/secret pair

This is **Waddle's current production shape** today:
- One `apiKeys` map in the Helm chart, populated with exactly one pair in
  practice (`infrastructure/waddle.cloud/charts/livekit-sfu/values.yaml`,
  `templates/configmap.yaml`).
- One webhook `apiKey` + `urls` list
  (`webhook.apiKey`, `webhook.urls` in the same chart).
- One shared TURN secret (`turn.secretName` → `LIVEKIT_TURN_SHARED_SECRET`).
- `mint_join_token` (`server/crates/waddle-sfu/src/token.rs`) mints a
  `VideoGrant{room_join, room, can_publish, can_subscribe,
  can_publish_data}` scoped to a single room name (the `CallId`) with no
  `roomAdmin`/`roomCreate`/`roomList`/`ingressAdmin` grants and no tenant
  claim anywhere in the JWT (`Claims{iss, sub, iat, nbf, exp, jti, video}`,
  same file).
- `SfuConfig::from_env` (`server/crates/waddle-sfu/src/config.rs`) loads
  exactly one key/secret/webhook-secret/TURN-secret process-wide — there is
  no concept of "which tenant" anywhere in this config today.
- `verify_webhook_signature` (`server/crates/waddle-sfu/src/webhook.rs`)
  verifies against exactly one shared `ApiSecret`; the only room-identifying
  field on the parsed event is `RoomInfo.name`, which is Waddle's `CallId`
  (a MUC room JID for group calls, or `<bare-jid>::<sid>` for 1:1 calls).

### Scoping/attribution/isolation under (b)
- **Token scoping**: identical mechanics to (a) but with one shared secret
  — a compromised token-minting path or leaked secret is a platform-wide
  blast radius, not a per-tenant one. `VideoGrant.room` is the *only* thing
  narrowing a token's reach today, and it is a client-presented string
  matched by LiveKit server-side against the actual room being joined — it
  does not itself prove which tenant's room is being addressed except by the
  string's own naming convention (`CallId` shape).
- **Webhook attribution**: same as (a)'s conclusion — the `RoomInfo.name`
  string is the only signal, and it is exactly what Waddle already parses
  (`CallId`). Under (b), the webhook signer/verifier secret is identical
  for every tenant, so there is no cryptographic tenant signal at all,
  only the room-name string — this is the weakest attribution story of the
  three patterns.
- **Egress**: same per-request `S3Upload` fields as (a) are available; (b)
  does not change egress design at all — egress isolation is orthogonal to
  the key/room-naming choice.
- **TURN**: single shared secret for all tenants, identical to (a)'s
  default state (Waddle's current TURN setup already matches this).
- **Noisy neighbor**: identical to (a) — no LiveKit-native per-tenant limit;
  Waddle's only current protection is the Kubernetes pod resource
  request/limit in the chart (cpu 500m/2, memory 512Mi/2Gi).
- **Usage metering**: identical to (a) in mechanism (webhooks + Prometheus +
  metadata), but tenant attribution is *entirely* dependent on parsing the
  `CallId` string correctly and consistently, with zero cryptographic
  corroboration.

### Why (b) alone is the riskiest long-term choice
Pattern (b) is what Waddle already runs, is the cheapest to keep, and is
"good enough" while there is one implicit tenant (a single community/org).
The moment there are genuinely distinct tenants (separate orgs on shared
infra) whose isolation matters for trust/compliance reasons, (b) alone gives
**no cryptographic separation** — every tenant's client holds a JWT signed
by the same secret, and any code path that fails to check the `room` claim
correctly, or any bug that lets a token's `room` grant be satisfied by an
unintended room, breaks isolation for all tenants at once, not just one.

---

## Pattern (c): per-tenant SFU deployments (separate clusters per tenant)

### Token/VideoGrant scoping
- No difference in VideoGrant *shape* from (a)/(b) — same fields, same
  `mint_join_token` logic could be reused verbatim. The isolation comes
  entirely from **physical/network separation**: tenant X's JWT is only ever
  verifiable against tenant X's `livekit-server` process, because that
  process holds only tenant X's key(s) (`keys:` map scoped to one tenant per
  deployment). A token minted for tenant X cannot even reach tenant Y's
  cluster to attempt a match unless network-routed there.

### Webhook attribution
- Trivial and unambiguous: each tenant's cluster has its own `webhook.api_key`
  and its own `urls` list — the receiving endpoint itself is a de-facto
  tenant discriminator (a distinct URL/deployment per tenant), so `iss` or
  room-name parsing become defense-in-depth rather than the sole mechanism.
  This is the only pattern where tenant attribution does not rely on parsing
  `event.room.name`/`event.room.metadata` at all.

### Egress isolation
- Egress can be given a **fully dedicated** IAM boundary: a per-tenant
  egress deployment (or per-tenant static config defaults) plus per-tenant
  buckets/roles via the same `S3Upload` per-request fields described under
  (a) — no shared code path, no shared static credential ever touches
  another tenant's data by construction.

### TURN credential separation
- Each tenant cluster can run (or point at) its own TURN endpoint with its
  own secret — the cleanest native fit for "per-tenant TURN secret" of the
  three patterns, since `turn:`/`rtc.turn_servers` config is already
  per-deployment.

### Noisy-neighbor controls
- The strongest of the three: a tenant's `limit:`/`room:` node config and
  Kubernetes resource ceiling apply to *only that tenant's* traffic. A
  tenant that saturates `bytes_per_sec` or `num_tracks` cannot affect any
  other tenant's cluster. This converts LiveKit's admittedly weak
  node-level-only limit primitives into an effective per-tenant limit, by
  construction rather than by LiveKit feature.

### Per-tenant usage metering
- Free: Prometheus metrics, webhook streams, and logs are already
  partitioned by cluster/deployment; "which tenant" is answered by "which
  cluster emitted this," with no string-parsing or key-lookup needed.

### Cost/ops reality (self-hosted)
- Every tenant needs: its own `livekit-server` (or HA set + Redis for
  clustering, per `config-sample.yaml`'s `redis:` block, which the doc says
  is required for "fully distributed" multi-node operation), its own TURN
  endpoint, its own egress fleet (or dedicated config), its own node-selector
  tuning, its own Prometheus scrape target. LiveKit provides **no
  tenant-router/fleet-manager** to place a call in "the right" tenant
  cluster — Waddle would have to build that dispatch layer (e.g. resolve
  tenant → cluster URL + key pair before minting a token), which is real,
  non-trivial infrastructure work with no LiveKit-native shortcut.
- This is the only pattern whose infra bill scales roughly linearly with
  tenant count rather than with total traffic — appropriate for a small
  number of large/sensitive tenants, uneconomical for many small tenants.

---

## Hard-to-reverse choices

1. **API key scheme is baked into every minted token and every webhook
   verifier the moment it ships.** Once thousands of tokens have been minted
   under a single shared key/secret (pattern b, Waddle's status quo) and
   thousands of webhook consumers assume "verify against exactly one
   secret" (Waddle's `verify_webhook_signature` today verifies against
   exactly one shared `ApiSecret`, per `server/crates/waddle-sfu/src/webhook.rs`),
   migrating to per-tenant keys (pattern a) requires a **coordinated cutover**:
   new tokens must carry the new tenant key's `iss`, the verifier must learn
   to resolve secrets by key (already how LiveKit's own reference verifier
   works — `provider.GetSecret(v.APIKey())` — so this is a straightforward
   extension, not a redesign), and any code that assumed "the" secret is a
   singleton (Waddle's `SfuConfig::from_env` today loads exactly one
   `LIVEKIT_API_KEY`/`LIVEKIT_API_SECRET`/`LIVEKIT_WEBHOOK_SECRET`/
   `LIVEKIT_TURN_SHARED_SECRET` as process-wide config) must become a
   tenant-keyed lookup instead. This is a moderate, not catastrophic,
   migration if done before the "thousands of tokens" mark — doing it after
   large-scale adoption forces either a flag-day or a long dual-verification
   window. **Recommendation: adopt per-tenant keys before there is
   meaningful multi-tenant traffic, since it only gets more expensive to
   retrofit.**

2. **Room-namespacing conventions baked into deployed clients and server
   code are expensive to change.** Waddle's `CallId` shape (a MUC room JID
   for group calls, or `<bare-jid>::<sid>` for 1:1 calls) is already
   deployed in native clients and server code, and is the *only*
   tenant-identifying signal available on a LiveKit webhook envelope (no
   LiveKit-native tenant field exists on `WebhookEvent`, confirmed above).
   If Waddle later wants a *cluster-routable* or *cryptographically
   verifiable* tenant tag baked into the room name (e.g. a tenant prefix
   segment), every existing client and every piece of server code that
   parses `CallId` needs to agree on the new shape simultaneously — this is
   a wire-format change across XMPP-native clients, not merely a LiveKit
   config change. **Recommendation: if a tenant-routable room-name
   convention is wanted for future pattern-(c) routing, decide the shape now
   while `CallId` is still a single-tenant construct — retrofitting a tenant
   segment into an already-multi-client-deployed ID format is the single
   most costly reversal risk in this whole comparison.**

3. **Single-cluster room-namespace choice (pattern b alone) affects blast
   radius irreversibly for data already processed.** Any security incident,
   token leak, or verification bug that occurred while all tenants shared
   one signing secret cannot be retroactively scoped — every token ever
   minted under that secret was, cryptographically, equally trusted. Moving
   to per-tenant keys going forward does not un-blast past incidents.

4. **Single-vs-per-tenant SFU cluster is an infra/cost-model commitment,
   not just a config change.** Choosing (c) for a tenant means standing up
   Redis-backed clustering, TURN, egress, and node-selector config
   per-tenant (all confirmed as per-deployment concerns in
   `config-sample.yaml`) — reversing back to a shared cluster later means
   migrating that tenant's live rooms/recordings/webhooks to new
   endpoints/keys, which is operationally disruptive mid-flight (in-call
   rooms cannot be silently moved between `livekit-server` clusters).
   **Recommendation: keep (c) reserved for tenants that need it from day
   one of onboarding (e.g. a contractual EU-sub-region or dedicated-capacity
   requirement), rather than moving an already-onboarded shared-cluster
   tenant to a dedicated cluster later.**

5. **EU data-residency is not reversible after the fact for egress
   recordings already written to a non-EU bucket/region.** Because
   `S3Upload.region`/`endpoint`/`bucket` are per-request fields (confirmed
   from the `livekit_egress.proto` primary source), the *decision* of which
   bucket/region a tenant's recordings land in is cheap to change forward,
   but files already written to a US-region bucket cannot be
   retroactively "made EU-resident" without a data migration and does not
   retroactively satisfy a compliance requirement for the period before the
   change. **Recommendation: settle the egress bucket/region-per-tenant
   design before any tenant starts recording, since this is a data
   placement decision, not a config toggle that can be silently
   backdated.**

---

## Sources

Primary (official LiveKit docs/code, fetched directly):
- `config-sample.yaml` (canonical config reference) —
  https://raw.githubusercontent.com/livekit/livekit/master/config-sample.yaml
  — full content fetched via `curl`; used for `keys:` map, `webhook:` block
  (single `api_key`), `turn:` block, `rtc.turn_servers` list, `room:`
  defaults, `limit:` node-level quotas, `redis:` clustering requirement,
  `ingress:` base URLs, `node_selector`/`region` fields.
- LiveKit Tokens & grants doc —
  https://docs.livekit.io/frontends/authentication/tokens/ — VideoGrant
  field list and meanings, JWT signing description.
- LiveKit `VideoGrant` JS SDK reference —
  https://docs.livekit.io/reference/server-sdk-js/interfaces/VideoGrant.html
  — cross-check of grant fields.
- LiveKit `auth` package (Go) —
  https://pkg.go.dev/github.com/livekit/protocol/auth — cross-check of
  `VideoGrant` struct fields (`canSubscribeMetrics` present in Go SDK).
- LiveKit webhooks doc —
  https://docs.livekit.io/home/server/webhooks/ — webhook signing
  mechanism (`Authorization` header, sha256 payload hash), full event-type
  list (`room_started`, `room_finished`, `participant_joined`,
  `participant_left`, `participant_connection_aborted`, `track_published`,
  `track_unpublished`, `egress_started`, `egress_updated`, `egress_ended`,
  `ingress_started`, `ingress_ended`), envelope fields (`id`, `createdAt`,
  `event` — confirmed no tenant/org field).
- `github.com/livekit/protocol`, `webhook/verifier.go` —
  https://raw.githubusercontent.com/livekit/protocol/main/webhook/verifier.go
  — read in full; proves webhook verification resolves the secret by the
  API key parsed from the JWT `iss` claim (`auth.ParseAPIToken` →
  `v.APIKey()` → `provider.GetSecret(...)`), confirming multi-key webhook
  *verification* (not signing) is natively supported.
- `github.com/livekit/protocol`, `protobufs/livekit_egress.proto` —
  https://github.com/livekit/protocol/blob/main/protobufs/livekit_egress.proto
  — `S3Upload`, `GCPUpload`, `AzureBlobUpload` message definitions fetched
  and read directly (grep of the file), confirming per-request
  `access_key`/`secret`/`assume_role_arn`/`region`/`endpoint`/`bucket`
  fields.
- LiveKit egress overview doc —
  https://docs.livekit.io/home/egress/overview/ — egress service types
  (RoomComposite, Web, Participant, TrackComposite, Track, Auto); did not
  itself enumerate S3 config (redirected to proto source above).
- LiveKit self-hosting ports/firewall doc —
  https://docs.livekit.io/home/self-hosting/ports-firewall/ — TURN/UDP and
  TURN/TLS port requirements; did not detail TURN authentication (config
  reference above is the authority for that).
- LiveKit self-hosting VM/Docker Compose guide —
  https://docs.livekit.io/home/self-hosting/vm/ — confirmed no multi-tenant
  or shared-secret guidance present in this doc.
- LiveKit self-hosted deployments overview —
  https://docs.livekit.io/deploy/custom/deployments/ — surfaced via search,
  general self-hosting entry point (not separately content-verified beyond
  title/context).

Secondary/community (lower confidence, flagged as such, used only for
context — no factual claim above rests solely on these):
- Prodinit, "Self-Hosted LiveKit: Production Architecture" —
  https://prodinit.com/blog/self-hosted-livekit-production-guide — a
  production run-book for a single-tenant 90K-calls/month deployment;
  fetched and confirmed it contains **no** multi-tenant, per-tenant-key, or
  noisy-neighbor guidance, so it was not used as a source for any claim
  above beyond "this pattern of doc exists and doesn't address
  multi-tenancy."
- Telnyx "multi-tenant self-hosted LiveKit platform" marketing page,
  surfaced via web search — not fetched/verified in depth; noted only as
  evidence that third-party multi-tenant hosting on top of LiveKit exists
  as a commercial pattern, not used for any technical claim.
- `anguzo/livekit-self-hosted` GitHub repo and `libreselfhosted.com/project/livekit`
  — surfaced via web search as community self-hosting resources; not
  fetched/read, listed only for completeness of the search trail.

Waddle repository facts (already known/provided, cited here for
completeness, not re-derived):
- `infrastructure/waddle.cloud/charts/livekit-sfu/values.yaml` and
  `templates/configmap.yaml` — single `livekit-server` deployment
  (`livekit/livekit-server:v1.11.0`), single `apiKeys` map in practice,
  single `webhook.apiKey`/`webhook.urls`, single `turn.secretName` →
  `LIVEKIT_TURN_SHARED_SECRET`, no S3/egress config, no room/participant
  limit or bandwidth config in the `livekit:` block, pod resources
  cpu 500m request/2 limit, memory 512Mi request/2Gi limit.
- `server/crates/waddle-sfu/src/token.rs` — `mint_join_token`, single-tenant
  `VideoGrant{room_join, room, can_publish, can_subscribe,
  can_publish_data}`, no `roomAdmin`/`roomCreate`/`roomList`/`ingressAdmin`,
  no tenant claim in `Claims{iss, sub, iat, nbf, exp, jti, video}`.
- `server/crates/waddle-sfu/src/config.rs` — `SfuConfig::from_env`, one
  `LIVEKIT_API_KEY`/`LIVEKIT_API_SECRET`/`LIVEKIT_WEBHOOK_SECRET` (defaults
  to `api_secret`)/`LIVEKIT_TURN_SHARED_SECRET`, process-wide singleton.
- `server/crates/waddle-sfu/src/webhook.rs` — `verify_webhook_signature`
  against one shared `ApiSecret`; `LiveKitWebhookEvent.RoomInfo.name` (the
  `CallId`) is the only room-identifying field; no tenant field anywhere.
- Wayfinder map issue #1489 — scopes this research to media-layer tenancy
  only; whole-platform (XMPP domain/storage/auth) multi-tenancy explicitly
  out of scope.
