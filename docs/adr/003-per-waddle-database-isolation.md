# ADR-003: Per-Waddle Database Isolation

**Status:** Accepted

**Date:** 2025-09-30

## Context

Waddles (equivalent to Discord servers) can range from small friend groups (5-10 members) to large communities (250,000+ members). We need a database architecture that:

- Scales from tiny to massive communities
- Provides strong data isolation between Waddles
- Simplifies compliance and data residency
- Enables per-Waddle features and customization
- Supports future self-hosting options

## Decision

Each Waddle gets its **own dedicated D1 database** for all Waddle-specific data (messages, channels, roles, settings).

### Database Structure

```
Central Directory DB (single D1)
├── waddles (metadata, owner, member_count)
├── waddle_members (membership mappings)
└── users (identity info)

Per-Waddle DB (one D1 per Waddle)
├── messages
├── channels
├── roles
├── threads
├── reactions
├── attachments
└── waddle_settings
```

### Database Provisioning

1. **On Waddle creation**: Provision new D1 database via Cloudflare API
2. **Run migrations**: Apply schema to new database
3. **Store reference**: Save D1 database ID in central directory
4. **Route requests**: Workers look up correct database per request

```typescript
// chat-worker/index.ts
export default {
  async fetch(request: Request, env: Env) {
    const waddleId = extractWaddleId(request);

    // Look up this Waddle's database
    const waddle = await env.CENTRAL_DB
      .prepare('SELECT d1_database_id FROM waddles WHERE id = ?')
      .bind(waddleId)
      .first();

    // Connect to Waddle-specific database
    const waddleDb = env[waddle.d1_database_id];

    // Execute queries against Waddle DB
    const messages = await waddleDb
      .prepare('SELECT * FROM messages WHERE channel_id = ?')
      .bind(channelId)
      .all();

    return Response.json(messages);
  },
};
```

## Consequences

### Positive

- **Data isolation**: Complete separation between Waddles
- **Compliance**: Easy to handle data deletion, export, residency requirements
- **Scale per Waddle**: Each Waddle has 10GB limit, not shared
- **Customization**: Per-Waddle schema extensions possible
- **Self-hosting**: Users can run their Waddle with their own database
- **Blast radius**: Database issues affect only one Waddle
- **Performance**: No cross-Waddle query interference
- **Simplicity**: Clear data boundaries, easier to reason about

### Negative

- **Provisioning overhead**: Must create D1 database for each Waddle
- **Connection management**: Workers must route to correct database
- **Cross-Waddle queries**: Cannot easily query across Waddles
- **Cost**: More databases = higher potential costs
- **Operational complexity**: More databases to monitor and maintain
- **Migration coordination**: Schema changes across thousands of databases

### Mitigation Strategies

- **Lazy provisioning**: Create database on first message, not Waddle creation
- **Database pooling**: Reuse connections where possible
- **Migration tooling**: Automated migration runner for all Waddles
- **Monitoring**: Aggregated metrics across all Waddle databases
- **Cost controls**: Archive inactive Waddles, consolidate tiny ones
- **Central index**: Maintain search index for cross-Waddle discovery

## Alternatives Considered

### Single Shared Database

**Pros:** Simpler, easier cross-Waddle queries, lower DB count
**Cons:** Scaling limits, security risks, compliance complexity, no self-hosting

**Rejected because:** 10GB limit would be hit quickly by large Waddles.

### Database Per Shard

**Pros:** Balance between isolation and management
**Cons:** Complex sharding logic, cross-shard queries, unclear boundaries

**Rejected because:** Adds complexity without clear benefits.

### External Database (Postgres/MySQL)

**Pros:** More mature, better tooling, higher limits
**Cons:** Not edge-native, latency, operational overhead, cost

**Rejected because:** Conflicts with Cloudflare-native architecture.

### Schema-based Multi-tenancy

**Pros:** Single database, simpler provisioning
**Cons:** D1 doesn't support schemas, weaker isolation

**Rejected because:** D1 limitation and security concerns.

## Implementation Details

### Central Directory Schema

```sql
-- Central directory (env.CENTRAL_DB)
CREATE TABLE waddles (
  id TEXT PRIMARY KEY,
  name TEXT UNIQUE NOT NULL,
  display_name TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  d1_database_id TEXT NOT NULL,  -- e.g., "waddle_abc123"
  member_count INTEGER DEFAULT 0,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  archived_at DATETIME,

  INDEX idx_owner (owner_id),
  INDEX idx_archived (archived_at)
);

CREATE TABLE waddle_members (
  waddle_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  role TEXT DEFAULT 'member',
  joined_at DATETIME DEFAULT CURRENT_TIMESTAMP,

  PRIMARY KEY (waddle_id, user_id),
  INDEX idx_user (user_id)
);
```

### Per-Waddle Schema

```sql
-- Per-Waddle database (env[waddle.d1_database_id])
CREATE TABLE messages (
  id TEXT PRIMARY KEY,
  channel_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  content TEXT,
  thread_id TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  edited_at DATETIME,
  deleted_at DATETIME,

  INDEX idx_channel_time (channel_id, created_at DESC),
  INDEX idx_thread (thread_id, created_at),
  INDEX idx_user (user_id, created_at DESC)
);

CREATE TABLE channels (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  type TEXT NOT NULL,  -- 'text', 'voice', 'announcement'
  position INTEGER,
  category_id TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE roles (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  permissions INTEGER NOT NULL,
  color TEXT,
  position INTEGER
);
```

### Database Provisioning Flow

```typescript
async function createWaddle(name: string, ownerId: string, env: Env) {
  // 1. Generate Waddle ID
  const waddleId = crypto.randomUUID();
  const dbName = `waddle_${waddleId.replace(/-/g, '')}`;

  // 2. Create D1 database via Cloudflare API
  const d1Response = await fetch(
    `https://api.cloudflare.com/client/v4/accounts/${env.ACCOUNT_ID}/d1/database`,
    {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${env.CF_API_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ name: dbName }),
    }
  );

  const { result: d1Database } = await d1Response.json();

  // 3. Run migrations on new database
  await runMigrations(d1Database.id, env);

  // 4. Store Waddle metadata in central DB
  await env.CENTRAL_DB.prepare(`
    INSERT INTO waddles (id, name, display_name, owner_id, d1_database_id)
    VALUES (?, ?, ?, ?, ?)
  `).bind(waddleId, name, name, ownerId, dbName).run();

  // 5. Add owner as first member
  await env.CENTRAL_DB.prepare(`
    INSERT INTO waddle_members (waddle_id, user_id, role)
    VALUES (?, ?, 'owner')
  `).bind(waddleId, ownerId).run();

  return { waddleId, d1DatabaseId: dbName };
}
```

### Database Routing Helper

```typescript
export async function getWaddleDb(waddleId: string, env: Env): Promise<D1Database> {
  // Cache lookup result
  const cached = await env.WADDLE_DB_CACHE.get(waddleId);
  if (cached) {
    return env[cached];
  }

  // Query central DB
  const waddle = await env.CENTRAL_DB
    .prepare('SELECT d1_database_id FROM waddles WHERE id = ?')
    .bind(waddleId)
    .first();

  if (!waddle) {
    throw new Error(`Waddle not found: ${waddleId}`);
  }

  // Cache for 5 minutes
  await env.WADDLE_DB_CACHE.put(waddleId, waddle.d1_database_id, {
    expirationTtl: 300,
  });

  return env[waddle.d1_database_id];
}
```

## Migration Strategy

### Phase 1: Proof of Concept
- Create 10 test Waddles with separate databases
- Validate performance and routing
- Test migration tooling

### Phase 2: Early Access
- Limited to 100 Waddles
- Monitor provisioning time and costs
- Gather feedback on isolation benefits

### Phase 3: Scale Testing
- Create 1,000+ Waddles
- Test migration across all databases
- Validate monitoring and alerting

### Phase 4: General Availability
- Unlimited Waddle creation
- Automated provisioning and migrations
- Self-service database management

## Monitoring

Track these metrics per Waddle and in aggregate:

- Database size and growth rate
- Query latency (p50, p95, p99)
- Connection errors
- Migration success rate
- Provisioning time
- Cost per Waddle

## References

- [D1 Databases](https://developers.cloudflare.com/d1/)
- [D1 Limits](https://developers.cloudflare.com/d1/platform/limits/)
- [Multi-tenancy Patterns](https://docs.aws.amazon.com/wellarchitected/latest/saas-lens/tenant-isolation.html)