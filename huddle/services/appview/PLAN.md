# AppView Worker

## Purpose

The main API surface for Huddle, handling XRPC endpoints, coordinating Durable Objects, and managing queues. This is the central orchestrator for all booking operations.

## Architecture

- **Framework**: Hono for routing and middleware
- **Runtime**: Cloudflare Workers
- **State**: Durable Objects for coordination
- **Data**: D1 for relational data, KV for caching, R2 for files
- **Async**: Queues for background processing

## Migrations

Local migrations in `/migrations`:

1. **0001_users.sql**
   ```sql
   CREATE TABLE users (
     id TEXT PRIMARY KEY,            -- DID (did:plc:...)
     handle TEXT UNIQUE,
     created_at INTEGER NOT NULL DEFAULT (unixepoch()),
     updated_at INTEGER NOT NULL DEFAULT (unixepoch())
   );
   ```

2. **0002_connectors.sql**
   ```sql
   CREATE TABLE connectors (
     id TEXT PRIMARY KEY,
     user_id TEXT NOT NULL REFERENCES users(id),
     provider TEXT NOT NULL CHECK (provider IN ('google','microsoft')),
     enc_refresh_token BLOB NOT NULL,
     access_token_expires_at INTEGER NOT NULL,
     scopes TEXT NOT NULL,
     last_sync_at INTEGER,
     created_at INTEGER NOT NULL DEFAULT (unixepoch())
   );
   ```

3. **0003_offers_index.sql**
   ```sql
   CREATE TABLE offers_index (
     uri TEXT PRIMARY KEY,           -- at://did/collection/rkey
     host_id TEXT NOT NULL REFERENCES users(id),
     start INTEGER NOT NULL,
     end INTEGER NOT NULL,
     policy TEXT NOT NULL,
     tz TEXT,
     note TEXT,
     indexed_at INTEGER NOT NULL DEFAULT (unixepoch())
   );
   CREATE INDEX idx_offers_host_time ON offers_index(host_id, start, end);
   ```

4. **0004_bookings.sql**
   ```sql
   CREATE TABLE bookings (
     id TEXT PRIMARY KEY,
     host_id TEXT NOT NULL REFERENCES users(id),
     guest_id TEXT NOT NULL REFERENCES users(id),
     status TEXT NOT NULL CHECK (status IN ('pending','held','confirmed','canceled','rescheduled')),
     hold_expires_at INTEGER,
     start INTEGER,
     end INTEGER,
     tz TEXT,
     ics_r2_key TEXT,
     created_at INTEGER NOT NULL DEFAULT (unixepoch())
   );
   ```

5. **0005_idempotency.sql**
   ```sql
   CREATE TABLE idempotency_keys (
     key TEXT PRIMARY KEY,
     actor_id TEXT,
     first_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
     response_code INTEGER,
     response_body TEXT
   );
   ```

## Routes

### Health & Status
- `GET /healthz` - Service health check
- `GET /zpages` - Observability pages

### XRPC Endpoints
- `POST /xrpc/com.huddle.match` - Find matching slots
- `POST /xrpc/com.huddle.finalize` - Confirm booking
- `GET /xrpc/com.huddle.listOffers` - List host's offers
- `POST /xrpc/com.huddle.createOffer` - Create new offer
- `POST /xrpc/com.huddle.deleteOffer` - Remove offer

### Internal Routes
- `POST /internal/sync-connector` - Trigger calendar sync
- `GET /internal/metrics` - Prometheus metrics

## Durable Objects

### UserConnectorDO
Manages calendar provider integrations per user.

**State:**
- Encrypted OAuth tokens
- Free/busy cache (5-minute TTL)
- Rate limit counters

**Methods:**
- `/freebusy` - Get availability
- `/tentative-hold` - Place calendar hold
- `/confirm-hold` - Confirm calendar event
- `/refresh-token` - Refresh OAuth token

### HostMatchDO
Computes matching candidates for a host.

**State:**
- Active hold mappings
- Candidate cache (per request)

**Methods:**
- `/hold` - Find matches and place holds
- `/release` - Release expired holds
- `/status` - Check hold status

### BookingDO
Manages booking lifecycle state machine.

**State:**
- Booking details
- State history
- ICS generation cache

**Methods:**
- `/create` - Initialize booking
- `/finalize` - Confirm booking
- `/cancel` - Cancel booking
- `/reschedule` - Change time

### RateLimiterDO
Token bucket rate limiting per IP/user.

**State:**
- Token count
- Last refill timestamp

**Methods:**
- `/allow` - Check if request allowed
- `/reset` - Reset limits (admin)

## Middleware

### idempotency.ts
Handles idempotent requests using `Idempotency-Key` header.
- Stores first response in D1
- Returns cached response on retry
- 24-hour key expiry

### auth.ts
Validates ATProto authentication.
- Verifies DID tokens
- Resolves handles
- Populates request context

### ratelimit.ts
Applies rate limiting via RateLimiterDO.
- Per-IP limits for anonymous
- Per-DID limits for authenticated
- Bypass for trusted services

## Scheduled Tasks (Cron)

### scheduled.ts
Handles periodic maintenance tasks.

**Jobs:**
- `*/10 * * * *` - Expire held bookings
- `0 */4 * * *` - Refresh expiring OAuth tokens
- `15 * * * *` - Reindex drift correction
- `0 0 * * *` - Clean old idempotency keys

## Bindings

```toml
# wrangler.toml
name = "huddle-appview"
main = "src/index.ts"
compatibility_date = "2025-09-14"

[vars]
TURNSTILE_SECRET = "..."
OAUTH_ENC_KEY = "..."

[[d1_databases]]
binding = "DB"
database_name = "huddle-db"

[[kv_namespaces]]
binding = "KV_FEATURES"
binding = "KV_GRAPH"

[[r2_buckets]]
binding = "R2_FILES"
bucket_name = "huddle-files"

[[durable_objects.bindings]]
name = "UserConnectorDO"
name = "HostMatchDO"
name = "BookingDO"
name = "RateLimiterDO"

[[queues.producers]]
binding = "Q_CALENDAR"
binding = "Q_NOTIFY"
binding = "Q_INDEX"

[[analytics_engine_datasets]]
binding = "WAE_PRODUCT"
binding = "WAE_SRE"
```

## Error Handling

- Use typed errors from kernel
- Return appropriate HTTP status codes
- Log errors to WAE_SRE
- Implement exponential backoff for retries

## Security

- Validate all inputs with Zod
- Encrypt sensitive data at rest
- Use Turnstile for anonymous requests
- Implement CORS properly
- Never log tokens or PII

## Performance

- Cache social graph in KV (1-hour TTL)
- Use DO alarms for delayed operations
- Batch D1 queries where possible
- Implement request coalescing in DOs
- Use ETags for cache control

## Monitoring

Key metrics to track:
- Request latency (p50, p95, p99)
- Match success rate
- Hold-to-booking conversion
- OAuth refresh failures
- Queue depth and age

## Testing

- Unit tests for route handlers
- Integration tests with Miniflare
- DO state machine tests
- Load tests for matching algorithm
- Chaos testing for calendar APIs

## Development

```bash
# Install dependencies
bun install

# Run migrations
wrangler d1 migrations apply DB

# Start dev server
wrangler dev

# Run tests
bun test

# Deploy
wrangler deploy
```