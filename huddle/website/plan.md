# ATProto × Cloudflare: End‑to‑End Architecture & Build Plan

This canvas maps each ATProto subsystem to Cloudflare primitives and provides copy‑paste scaffolding: `wrangler.toml` layouts, D1 schema, Durable Object (DO) skeletons, Workers (AppView/API, Firehose Ingest, Webhooks, Notify), Queue consumers, Cron jobs, Pages/Next.js hooks, security/ops, and a 12‑week build plan.

---

## System overview

**ATProto (identity & data ownership)**

* Users authenticate via DID/handle.
* Public metadata lives in each user’s PDS using your lexicons.
* AppView enforces policy and performs private calendar matching.

**Cloudflare runtime**

* **Workers**: AppView/API, Firehose Ingest, Webhooks, Notify.
* **Durable Objects**: coordination/state (atomic holds, dedupe, rate limits).
* **Queues**: async jobs (calendar API calls, ICS fan‑out, retries, indexing).
* **D1**: relational metadata (users, connectors, booking indexes, audit).
* **KV**: hot caches, feature flags, session nonces.
* **R2**: ICS blobs, exports, audit bundles.
* **Cron Triggers**: token refresh, hold expiry sweeps, reindex drift.
* **Zero Trust / Access**: admin & connector consoles.
* **Turnstile**: throttle anonymous flows.
* **Workers Analytics Engine (WAE)**: product analytics & SLOs.
* **Pages**: Next.js front‑end (+ Functions for SSR/API where helpful).

---

## Resource map

**Workers**

* `appview-worker` — XRPC/REST, policy, DO service bindings, D1 reads/writes, Queue producers.
* `firehose-ingest-worker` — subscribes to Relay firehose, filters records, produces `index-tasks`.
* `webhooks-worker` — receives calendar provider webhooks, confirms holds, produces `calendar-tasks`.
* `notify-worker` — consumes `notify-tasks`, sends email/webhook/DM, reads ICS from R2.

**Durable Objects**

* `UserConnectorDO` (per user) — encrypted OAuth token metadata, throttled provider calls, free/busy cache.
* `HostMatchDO` (per host DID or host+day shard) — candidate generation & atomic holds.
* `BookingDO` (per booking) — confirm/cancel/reschedule serialization.
* `RateLimiterDO` (per requester or /24) — protects hosts from spam.

**Queues**

* `calendar-tasks`, `notify-tasks`, `index-tasks` (+ optional DLQs).

**Data Stores**

* **D1**: `users`, `connectors`, `offers_index`, `bookings`, `audit_events`, `idempotency_keys`.
* **KV**: `graph-cache`, `features`, `session-nonces`.
* **R2**: `ics/…`, `exports/…`.

**Other**

* Cron jobs: `refresh-oauth`, `expire-holds`, `reindex-drift`.
* WAE datasets: `product_metrics`, `sre_kpis`.

---

## Lexicons (ATProto) — minimal starters

> Place in `./lexicons/` and publish with your AppView.

```json
{
  "lexicon": 1,
  "id": "com.yourapp.slotOffer",
  "defs": {
    "main": {
      "type": "record",
      "description": "Coarse availability windows + policy tier",
      "record": {
        "key": "tid",
        "record": {
          "type": "object",
          "required": ["start", "end", "policy"],
          "properties": {
            "start": {"type": "string", "format": "datetime"},
            "end": {"type": "string", "format": "datetime"},
            "policy": {"type": "string", "enum": ["mutual","connected","follower","anyone"]},
            "tz": {"type": "string"},
            "note": {"type": "string", "maxLength": 280}
          }
        }
      }
    }
  }
}
```

```json
{
  "lexicon": 1,
  "id": "com.yourapp.bookingRequest",
  "defs": {
    "main": {
      "type": "record",
      "description": "Guest’s constraints & status",
      "record": {
        "key": "tid",
        "record": {
          "type": "object",
          "required": ["hostDid","constraints","status"],
          "properties": {
            "hostDid": {"type": "string"},
            "constraints": {
              "type": "object",
              "properties": {
                "durationMin": {"type": "integer"},
                "durationMax": {"type": "integer"},
                "earliest": {"type": "string", "format": "datetime"},
                "latest": {"type": "string", "format": "datetime"},
                "days": {"type": "array", "items": {"type": "string"}},
                "tz": {"type": "string"}
              }
            },
            "status": {"type": "string", "enum": ["pending","held","declined","confirmed"]},
            "offerUri": {"type": "string"},
            "note": {"type": "string", "maxLength": 280}
          }
        }
      }
    }
  }
}
```

```json
{
  "lexicon": 1,
  "id": "com.yourapp.booking",
  "defs": {
    "main": {
      "type": "record",
      "description": "Portable booking metadata (minimal public fields)",
      "record": {
        "key": "tid",
        "record": {
          "type": "object",
          "required": ["hostDid","guestDid","status"],
          "properties": {
            "hostDid": {"type": "string"},
            "guestDid": {"type": "string"},
            "status": {"type": "string", "enum": ["confirmed","canceled","rescheduled"]},
            "timeMasked": {"type": "boolean", "default": true},
            "icsR2Key": {"type": "string"}
          }
        }
      }
    }
  }
}
```

---

## `wrangler.toml` — multi‑worker, queues, DOs, D1, KV, R2, WAE, crons

> Duplicate/trim bindings per worker as needed. Use environment sections for `staging`/`production`.

```toml
name = "appview-worker"
main = "src/appview/index.ts"
compatibility_date = "2025-09-14"

[vars]
APPVIEW_ORIGIN = "https://app.yourapp.example"
TURNSTILE_SECRET = "${TURNSTILE_SECRET}"
OAUTH_ENC_KEY = "${OAUTH_ENC_KEY}"

[[kv_namespaces]]
binding = "KV_FEATURES"
id = "kv_features_id"

[[kv_namespaces]]
binding = "KV_GRAPH"
id = "kv_graph_id"

[[r2_buckets]]
binding = "R2_FILES"
bucket_name = "yourapp-bucket"

[[d1_databases]]
binding = "DB"
database_name = "yourapp-db"
id = "yourapp-db-id"

[[durable_objects.bindings]]
name = "UserConnectorDO"
class_name = "UserConnectorDO"

[[durable_objects.bindings]]
name = "HostMatchDO"
class_name = "HostMatchDO"

[[durable_objects.bindings]]
name = "BookingDO"
class_name = "BookingDO"

[[durable_objects.bindings]]
name = "RateLimiterDO"
class_name = "RateLimiterDO"

[[queues.producers]]
binding = "Q_CALENDAR"
queue = "calendar-tasks"

[[queues.producers]]
binding = "Q_NOTIFY"
queue = "notify-tasks"

[[queues.producers]]
binding = "Q_INDEX"
queue = "index-tasks"

[[analytics_engine_datasets]]
binding = "WAE_PRODUCT"
dataset = "product_metrics"

[[analytics_engine_datasets]]
binding = "WAE_SRE"
dataset = "sre_kpis"

[triggers]
crons = [
  "0 */4 * * *",     # refresh-oauth every 4h
  "*/10 * * * *",    # expire-holds sweep every 10m
  "15 * * * *"       # reindex-drift hourly at :15
]

# --- Firehose Ingest Worker ---
[[services]]
binding = "APPVIEW"
service = "appview-worker"

[observability] # optional traces/logpush as desired

# Secondary worker entries (use separate wrangler files or multi-project monorepo):
# firehose-ingest-worker, webhooks-worker, notify-worker
```

**`wrangler.firehose.toml` (separate project)**

```toml
name = "firehose-ingest-worker"
main = "src/firehose/index.ts"
compatibility_date = "2025-09-14"

[[queues.producers]]
binding = "Q_INDEX"
queue = "index-tasks"

[[d1_databases]]
binding = "DB"
database_name = "yourapp-db"
id = "yourapp-db-id"
```

**`wrangler.notify.toml`**

```toml
name = "notify-worker"
main = "src/notify/index.ts"
compatibility_date = "2025-09-14"

[[queues.consumers]]
queue = "notify-tasks"
binding = "Q_NOTIFY"
max_batch_size = 50
max_retries = 10

[[r2_buckets]]
binding = "R2_FILES"
bucket_name = "yourapp-bucket"
```

**`wrangler.webhooks.toml`**

```toml
name = "webhooks-worker"
main = "src/webhooks/index.ts"
compatibility_date = "2025-09-14"

[[queues.producers]]
binding = "Q_CALENDAR"
queue = "calendar-tasks"

[[d1_databases]]
binding = "DB"
database_name = "yourapp-db"
id = "yourapp-db-id"
```

---

## D1 schema (SQLite) — migrations

> Store under `migrations/0001_init.sql` and wire via `wrangler d1 migrations apply`.

```sql
-- users
CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY,            -- DID (did:plc:...)
  handle TEXT UNIQUE,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- connectors (Google/Microsoft)
CREATE TABLE IF NOT EXISTS connectors (
  id TEXT PRIMARY KEY,            -- cuid/uuid
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider TEXT NOT NULL CHECK (provider IN ('google','microsoft')),
  enc_refresh_token BLOB NOT NULL,
  access_token_expires_at INTEGER NOT NULL,
  scopes TEXT NOT NULL,
  last_sync_at INTEGER,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_connectors_user ON connectors(user_id);

-- offers index (denormalized from PDS for discovery/policy)
CREATE TABLE IF NOT EXISTS offers_index (
  uri TEXT PRIMARY KEY,           -- at:// did / rkey
  host_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  start INTEGER NOT NULL,
  end INTEGER NOT NULL,
  policy TEXT NOT NULL,
  tz TEXT,
  note TEXT,
  indexed_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_offers_host_time ON offers_index(host_id, start, end);

-- bookings
CREATE TABLE IF NOT EXISTS bookings (
  id TEXT PRIMARY KEY,            -- cuid/uuid
  host_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  guest_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  status TEXT NOT NULL CHECK (status IN ('pending','held','confirmed','canceled','rescheduled')),
  hold_expires_at INTEGER,
  start INTEGER,                  -- may be null while masked
  end INTEGER,
  tz TEXT,
  ics_r2_key TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX IF NOT EXISTS idx_bookings_host_time ON bookings(host_id, start);
CREATE INDEX IF NOT EXISTS idx_bookings_guest_time ON bookings(guest_id, start);

-- idempotency (per endpoint)
CREATE TABLE IF NOT EXISTS idempotency_keys (
  key TEXT PRIMARY KEY,
  actor_id TEXT,
  first_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
  response_code INTEGER,
  response_body TEXT
);

-- audit trail pointers (R2 paths)
CREATE TABLE IF NOT EXISTS audit_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subject_id TEXT NOT NULL,       -- booking id or user id
  kind TEXT NOT NULL,
  r2_key TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

---

## AppView/API Worker — Hono + XRPC + service bindings

`src/appview/index.ts`

```ts
import { Hono } from 'hono'
import { z } from 'zod'

export type Env = {
  DB: D1Database
  KV_FEATURES: KVNamespace
  KV_GRAPH: KVNamespace
  R2_FILES: R2Bucket
  Q_CALENDAR: Queue<any>
  Q_NOTIFY: Queue<any>
  Q_INDEX: Queue<any>
  UserConnectorDO: DurableObjectNamespace
  HostMatchDO: DurableObjectNamespace
  BookingDO: DurableObjectNamespace
  RateLimiterDO: DurableObjectNamespace
  WAE_PRODUCT: AnalyticsEngineDataset
  WAE_SRE: AnalyticsEngineDataset
  TURNSTILE_SECRET: string
  OAUTH_ENC_KEY: string
}

const app = new Hono<{ Bindings: Env }>()

// Health
app.get('/healthz', (c) => c.json({ ok: true }))

// Turnstile verify helper
async function verifyTurnstile(secret: string, token: string, ip?: string) {
  const form = new FormData()
  form.append('secret', secret)
  form.append('response', token)
  if (ip) form.append('remoteip', ip)
  const r = await fetch('https://challenges.cloudflare.com/turnstile/v0/siteverify', { method: 'POST', body: form })
  const data = await r.json<any>()
  return !!data.success
}

// Idempotency middleware
app.use('*', async (c, next) => {
  const key = c.req.header('Idempotency-Key')
  if (!key) return next()
  const row = await c.env.DB.prepare('SELECT response_code, response_body FROM idempotency_keys WHERE key=?').bind(key).first()
  if (row) return c.newResponse(row.response_body as string, { status: row.response_code as number })
  await next()
})

// XRPC example: compute candidates & place holds
app.post('/xrpc/com.yourapp.match', async (c) => {
  const body = await c.req.json()
  // Rate limit
  const rlId = c.env.RateLimiterDO.idFromName(c.req.header('cf-connecting-ip') ?? 'anon')
  const rl = c.env.RateLimiterDO.get(rlId)
  const allowed = await (await rl.fetch('https://do/allow')).json<any>()
  if (!allowed.ok) return c.json({ error: 'rate_limited' }, 429)

  // Validate Turnstile for anonymous
  if (body.turnstileToken) {
    const ok = await verifyTurnstile(c.env.TURNSTILE_SECRET, body.turnstileToken, c.req.header('cf-connecting-ip') || undefined)
    if (!ok) return c.json({ error: 'bot' }, 400)
  }

  // Delegate to HostMatchDO
  const hostId = c.env.HostMatchDO.idFromName(body.hostDid)
  const host = c.env.HostMatchDO.get(hostId)
  const res = await host.fetch('https://do/hold', { method: 'POST', body: JSON.stringify(body) })
  return new Response(res.body, res)
})

// Booking finalize
app.post('/xrpc/com.yourapp.finalize', async (c) => {
  const body = await c.req.json()
  const id = c.env.BookingDO.idFromName(body.bookingId)
  const bo = c.env.BookingDO.get(id)
  const res = await bo.fetch('https://do/finalize', { method: 'POST', body: JSON.stringify(body) })
  return new Response(res.body, res)
})

export default app
```

---

## Durable Objects — class skeletons (atomic holds, rate limit, booking)

`src/appview/dos.ts`

```ts
export class RateLimiterDO implements DurableObject {
  constructor(readonly state: DurableObjectState, readonly env: Env) {}
  async fetch(req: Request) {
    const url = new URL(req.url)
    if (url.pathname !== '/allow') return new Response('not found', { status: 404 })
    const now = Date.now()
    const key = 'bucket'
    const { tokens = 10, ts = now } = (await this.state.storage.get<any>(key)) || {}
    const refill = Math.floor((now - ts) / 1000) // 1 token/sec
    const newTokens = Math.min(10, tokens + refill)
    const allowed = newTokens > 0
    await this.state.storage.put(key, { tokens: allowed ? newTokens - 1 : newTokens, ts: now })
    return Response.json({ ok: allowed })
  }
}

export class UserConnectorDO implements DurableObject {
  constructor(readonly state: DurableObjectState, readonly env: Env) {}
  async fetch(req: Request) {
    const url = new URL(req.url)
    if (url.pathname === '/freebusy') {
      const body = await req.json<any>()
      // TODO: decrypt refresh token, exchange, call provider, cache briefly in storage
      return Response.json({ slots: [] })
    }
    if (url.pathname === '/tentative-hold') {
      // TODO: place tentative hold via provider API (enqueue via Q_CALENDAR for retries)
      return Response.json({ ok: true })
    }
    return new Response('not found', { status: 404 })
  }
}

export class HostMatchDO implements DurableObject {
  constructor(readonly state: DurableObjectState, readonly env: Env) {}
  async fetch(req: Request) {
    const url = new URL(req.url)
    if (url.pathname === '/hold' && req.method === 'POST') {
      const body = await req.json<any>()
      const { hostDid, guestDid, constraints } = body

      // 1) Compute candidates (read KV_GRAPH for relationship tiers; consult D1 offers_index)
      // 2) For each candidate, call UserConnectorDO for both host/guest tentative holds
      // 3) If both holds succeed, persist booking as held with expiry

      const bookingId = crypto.randomUUID()
      await this.env.DB.prepare(
        'INSERT INTO bookings (id, host_id, guest_id, status, hold_expires_at) VALUES (?,?,?,?,?)'
      ).bind(bookingId, hostDid, guestDid, 'held', Math.floor(Date.now()/1000)+900).run()

      // Demonstrate queue usage (actual provider calls should be enqueued)
      await this.env.Q_CALENDAR.send({ type: 'tentative-hold', bookingId })

      return Response.json({ bookingId, status: 'held' })
    }
    return new Response('not found', { status: 404 })
  }
}

export class BookingDO implements DurableObject {
  constructor(readonly state: DurableObjectState, readonly env: Env) {}
  async fetch(req: Request) {
    const url = new URL(req.url)
    if (url.pathname === '/finalize' && req.method === 'POST') {
      const body = await req.json<any>()
      const { bookingId, start, end, tz } = body
      await this.env.DB.prepare(
        'UPDATE bookings SET status="confirmed", start=?, end=?, tz=?, updated_at=unixepoch() WHERE id=?'
      ).bind(start, end, tz, bookingId).run()

      // Generate ICS to R2
      const ics = generateICS({ uid: bookingId, start, end, tz })
      const r2Key = `ics/${bookingId}.ics`
      await this.env.R2_FILES.put(r2Key, ics)
      await this.env.DB.prepare('UPDATE bookings SET ics_r2_key=? WHERE id=?').bind(r2Key, bookingId).run()

      await this.env.Q_NOTIFY.send({ type: 'booking-confirmed', bookingId, r2Key })
      return Response.json({ ok: true, bookingId })
    }
    return new Response('not found', { status: 404 })
  }
}

function generateICS({ uid, start, end, tz }: { uid: string; start: number; end: number; tz: string }) {
  const toICSDate = (sec: number) => new Date(sec * 1000).toISOString().replace(/[-:]/g, '').split('.')[0] + 'Z'
  return [
    'BEGIN:VCALENDAR',
    'VERSION:2.0',
    'PRODID:-//yourapp//EN',
    'BEGIN:VEVENT',
    `UID:${uid}`,
    `DTSTAMP:${toICSDate(Math.floor(Date.now()/1000))}`,
    `DTSTART:${toICSDate(start)}`,
    `DTEND:${toICSDate(end)}`,
    'SUMMARY:Meeting',
    'END:VEVENT',
    'END:VCALENDAR'
  ].join('\r\n')
}
```

> Export these DO classes from `src/appview/dos.ts` and re‑export in `src/appview/index.ts` if you keep everything in one worker. Update `wrangler.toml` DO bindings accordingly.

---

## Firehose Ingest Worker — subscribe & normalize

`src/firehose/index.ts`

```ts
export interface Env { Q_INDEX: Queue<any>; DB: D1Database }

export default {
  async fetch(req: Request, env: Env) {
    if (new URL(req.url).pathname !== '/healthz') return new Response('ok')
    return new Response('ok')
  },
  async scheduled(event: ScheduledEvent, env: Env, ctx: ExecutionContext) {
    // Optional: cron to reconnect if needed
  }
}

// Pseudo: connect to Relay firehose (WS or HTTP stream) and filter records
// For each com.yourapp.slotOffer / bookingRequest, send to Q_INDEX for upsert into D1
```

**Index consumer (can live in AppView or a tiny worker)**

`src/indexer/consumer.ts`

```ts
export default {
  async queue(batch: MessageBatch<any>, env: Env, ctx: ExecutionContext) {
    for (const msg of batch.messages) {
      const e = msg.body
      if (e.type === 'slotOffer') {
        await env.DB.prepare(
          'INSERT OR REPLACE INTO offers_index (uri, host_id, start, end, policy, tz, note, indexed_at) VALUES (?,?,?,?,?,?,?,unixepoch())'
        ).bind(e.uri, e.hostDid, e.start, e.end, e.policy, e.tz ?? null, e.note ?? null).run()
      }
      // handle bookingRequest, etc.
    }
  }
}
```

---

## Webhooks Worker — provider callbacks

`src/webhooks/index.ts`

```ts
export interface Env { Q_CALENDAR: Queue<any>; DB: D1Database }

export default {
  async fetch(req: Request, env: Env) {
    const url = new URL(req.url)
    // Verify provider signature (Google/Microsoft) here
    if (url.pathname === '/provider/notify') {
      const body = await req.json<any>()
      await env.Q_CALENDAR.send({ type: 'provider-event', body })
      return new Response('ok')
    }
    return new Response('not found', { status: 404 })
  }
}
```

---

## Notify Worker — email/webhook/DM with ICS fan‑out

`src/notify/index.ts`

```ts
export interface Env { R2_FILES: R2Bucket }

export default {
  async queue(batch: MessageBatch<any>, env: Env, ctx: ExecutionContext) {
    for (const m of batch.messages) {
      if (m.body.type === 'booking-confirmed') {
        const obj = await env.R2_FILES.get(m.body.r2Key)
        const ics = obj ? await obj.text() : ''
        // TODO: send email (provider API), webhook POST, or AT DM
      }
    }
  }
}
```

---

## Cron jobs — rotation & sweeps

`src/appview/scheduled.ts`

```ts
export default {
  async scheduled(event: ScheduledEvent, env: Env, ctx: ExecutionContext) {
    const min = new Date(event.scheduledTime).getUTCMinutes()
    if (min % 10 === 0) {
      // expire-holds
      await env.DB.prepare('UPDATE bookings SET status="pending" WHERE status="held" AND hold_expires_at < unixepoch()').run()
    }
    if (min % 60 === 0) {
      // reindex-drift or metrics rollups
      await env.WAE_SRE.writeDataPoint({ blobs: ['reindex_tick'] })
    }
    // refresh-oauth (every 4h): iterate connectors close to expiry and refresh
  }
}
```

---

## Pages (Next.js) — login, offers, booking inbox

**Structure**

```
/apps/web (Next.js 14+ App Router)
  /app
    /offers
    /inbox
    /connect
    /api/xrpc/[...path]/route.ts  # proxy to appview-worker or implement in Functions
  /components
  /lib
  /pages/api/turnstile-verify.ts  # (if using Pages Functions)
```

**Turnstile client snippet**

```html
<script src="https://challenges.cloudflare.com/turnstile/v0/api.js" async defer></script>
<div class="cf-challenge" data-sitekey="YOUR_SITE_KEY"></div>
```

**Proxy XRPC in Pages Function (optional)**

```ts
export const onRequestPost: PagesFunction = async (ctx) => {
  const res = await fetch(`${ctx.env.APPVIEW_ORIGIN}/xrpc/${ctx.params.path}`, {
    method: 'POST', body: await ctx.request.text(), headers: ctx.request.headers
  })
  return res
}
```

---

## Security & privacy

* **Zero Trust / Access**: gate `/admin` and `/connectors` routes; enforce device posture for ops.
* **Encrypted tokens**: store only encrypted refresh tokens in D1; rotate via cron.
* **Least privilege**: request minimal calendar scopes.
* **No global free/busy**: DOs call providers; exact times never exposed outside connector/match path.
* **Idempotency**: require `Idempotency-Key` on mutating endpoints; persist responses.

---

## Workers Analytics Engine — KPIs & SLOs

**Write example**

```ts
await env.WAE_PRODUCT.writeDataPoint({
  doubles: [time_to_match_ms],
  blobs: ['match'],
  indexes: [hostDidHash]
})
```

**Query sketch (via GraphQL/SQL in dashboard or API)**

```sql
SELECT bucket, avg(value) FROM WAE_PRODUCT
WHERE label = 'match' AND ts > now() - 24h
GROUP BY bucket
```

Metrics to track: `time_to_match_ms`, `hold_success_rate`, `provider_latency_ms`, `queue_age_s`, `do_restarts`.

---

## CI/CD

**GitHub Action (Wrangler + D1 migrations)**

```yaml
name: deploy
on: { push: { branches: [ main ] } }
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cloudflare/wrangler-action@v3
        with:
          apiToken: ${{ secrets.CF_API_TOKEN }}
          command: >-
            d1 migrations apply yourapp-db && wrangler deploy --minify src/appview/index.ts && \
            wrangler deploy --config wrangler.firehose.toml && \
            wrangler deploy --config wrangler.notify.toml && \
            wrangler deploy --config wrangler.webhooks.toml
```

---

## Matching & concurrency design (DOs)

* **Atomic holds**: `HostMatchDO` computes `N` candidates → enqueues provider tentative holds via `UserConnectorDO` (or direct queue) → success only when *both* calendars hold the same slot.
* **Sharding**: celebrity hosts → shard `HostMatchDO` by `(host, day)` and precompute candidates to KV.
* **Backpressure**: if quotas trip, shed to Queue with delay; return `202 Accepted` and polling token.

---

## 12‑week build plan

**Weeks 1–2 — Scaffolding & Lexicons**

* Define lexicons + XRPC.
* Spin up Pages (Next.js) + `appview-worker` with Wrangler.
* D1 migration + KV namespaces.

**Weeks 3–4 — Ingest & Index**

* Firehose Ingest → `index-tasks` → D1 upserts.
* Graph cache hydrator with TTL in KV.

**Weeks 5–6 — Connectors & Matching**

* OAuth apps (Google/Microsoft) behind Access.
* `UserConnectorDO` token store + free/busy.
* `HostMatchDO` candidates + `calendar-tasks` tentative holds.
* Turnstile + `RateLimiterDO`.

**Weeks 7–8 — Booking lifecycle**

* `BookingDO` confirm/decline/reschedule.
* ICS to R2; `notify-tasks`.
* Public XRPC/REST with idempotency keys.

**Week 9 — Ops & Security**

* Access policies; cron rotation & stale-hold cleanup.
* WAE dashboards (TTM, success, provider errors).

**Week 10 — UX polish**

* Pages SSR for offer composer & inbox; timezone UX.

**Week 11 — Load & chaos**

* Soak tests on DO contention; synthetic provider faults via DLQ.
* KV hit‑rate tuning; D1 query plans.

**Week 12 — GA hardening**

* Observability playbooks; error budgets.
* Export/portability via R2 bundles.

---

## Stretch options

* Vectorize slot personalization (embeddings per guest).
* Workers AI: summarize meeting intents from requests.

---

### Notes

* Replace placeholders (`yourapp`, secrets) with environment‑specific values.
* For DOs shared across workers, either keep all DO classes in `appview-worker` and reference via `script_name`, or centralize in a dedicated `state-worker`.
* Add DLQs to Queues for robust retries.
* Consider `Hono` routes per XRPC NSID for clarity.

