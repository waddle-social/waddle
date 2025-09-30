# ADR-004: Per-User View Storage

**Status:** Accepted

**Date:** 2025-09-30

## Context

Waddle's channel-less architecture requires flexible, user-defined views to surface relevant conversations. Different personas (help-seekers, contributors, lurkers, gamers) need different lenses on the same message stream. Users should be able to:

- Create custom views (e.g., "SG1 chat", "Star Wars discussions", "Help requests")
- Filter by tags, participants, time, content type
- Save and switch between multiple views
- Share views within a Waddle (admin-created shared views)

This personalization requires storing view definitions and preferences per user.

## Decision

Each user gets their **own dedicated D1 database** for storing:

- View definitions (filters, sort order, grouping rules)
- User preferences (theme, notifications, defaults)
- Personal metadata (bookmarks, read state, muted conversations)

### Database Structure

```
Per-User DB (one D1 per user)
├── views (view definitions)
├── view_filters (filter rules per view)
├── preferences (user settings)
├── bookmarks (saved messages)
├── read_state (last read timestamps per Waddle/channel)
└── muted_conversations (hidden threads)

Waddle DB (shared views)
├── shared_views (admin-created views)
└── shared_view_filters (filters for shared views)
```

### View Definition Model

```typescript
interface View {
  id: string;
  userId: string;
  waddleId: string;
  name: string;                    // "SG1 Chat"
  description?: string;
  icon?: string;
  isDefault: boolean;
  sortOrder: 'newest' | 'oldest' | 'relevance';
  groupBy: 'conversation' | 'time' | 'user' | 'tag';
  filters: ViewFilter[];
  createdAt: Date;
  updatedAt: Date;
}

interface ViewFilter {
  id: string;
  viewId: string;
  type: 'tag' | 'user' | 'content' | 'time' | 'channel';
  operator: 'includes' | 'excludes' | 'equals' | 'matches';
  value: string | string[];
}
```

### Example Views

```typescript
// "Support Requests" view for contributors
{
  name: "Support Requests",
  groupBy: "conversation",
  sortOrder: "newest",
  filters: [
    { type: "tag", operator: "includes", value: ["help", "support"] },
    { type: "user", operator: "excludes", value: [currentUserId] }
  ]
}

// "My Questions" view for help-seekers
{
  name: "My Questions",
  groupBy: "time",
  sortOrder: "newest",
  filters: [
    { type: "user", operator: "equals", value: [currentUserId] },
    { type: "tag", operator: "includes", value: ["help"] }
  ]
}

// "SG1 Chat" view for topic-focused users
{
  name: "SG1 Chat",
  groupBy: "conversation",
  sortOrder: "newest",
  filters: [
    { type: "tag", operator: "includes", value: ["sg1", "stargate"] },
    { type: "content", operator: "matches", value: "SG-?1|Stargate" }
  ]
}
```

## Consequences

### Positive

- **User autonomy**: Full control over conversation organization
- **Performance**: View filters execute locally without affecting others
- **Privacy**: User preferences never leak to other users
- **Flexibility**: Unlimited views per user, complex filter combinations
- **Scalability**: Each user's view storage scales independently
- **Personalization**: AI can suggest views based on individual behavior
- **Self-hosting**: Users can export and own their view data

### Negative

- **Database proliferation**: One DB per user adds operational overhead
- **Provisioning cost**: Creating databases for millions of users
- **Query complexity**: Applying filters requires joining across Waddle and user DBs
- **Consistency**: User DB and Waddle DB can become out of sync
- **Migration complexity**: Schema changes across all user databases
- **Cross-user features**: Cannot easily discover popular views

### Mitigation Strategies

- **Lazy provisioning**: Create user DB on first view creation, not signup
- **View templates**: Provide pre-built views, most users won't customize
- **Shared views**: Admin views stored in Waddle DB, reduce user DB usage
- **Materialized views**: Cache filtered results for common views
- **View analytics**: Track popular filters without accessing user DBs
- **Migration tooling**: Automated user DB migrations with rollback support

## Alternatives Considered

### Views in Waddle Database

**Pros:** Simpler, fewer databases, easier cross-user analytics
**Cons:** Privacy concerns, scaling limits, user data mixed with content

**Rejected because:** Privacy and scaling concerns outweigh simplicity.

### Views in Central Database

**Pros:** Single database, easy to manage
**Cons:** Single point of failure, 10GB limit hit quickly

**Rejected because:** Would become bottleneck at scale.

### Serverless Key-Value Store (KV)

**Pros:** Simple provisioning, low cost
**Cons:** No relational queries, poor for complex filters, eventual consistency

**Rejected because:** View filters require complex SQL queries.

### Client-Side Only

**Pros:** No server storage, maximum privacy
**Cons:** No sync across devices, lost on logout, cannot share

**Rejected because:** Multi-device support is essential.

## Implementation Details

### User Database Schema

```sql
-- Per-User DB (env[user_db_id])
CREATE TABLE views (
  id TEXT PRIMARY KEY,
  waddle_id TEXT NOT NULL,
  name TEXT NOT NULL,
  description TEXT,
  icon TEXT,
  is_default BOOLEAN DEFAULT false,
  sort_order TEXT DEFAULT 'newest',
  group_by TEXT DEFAULT 'conversation',
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
  updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,

  INDEX idx_waddle (waddle_id)
);

CREATE TABLE view_filters (
  id TEXT PRIMARY KEY,
  view_id TEXT NOT NULL,
  type TEXT NOT NULL,              -- 'tag', 'user', 'content', 'time'
  operator TEXT NOT NULL,          -- 'includes', 'excludes', 'equals'
  value TEXT NOT NULL,             -- JSON array or string

  FOREIGN KEY (view_id) REFERENCES views(id) ON DELETE CASCADE
);

CREATE TABLE preferences (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,             -- JSON
  updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE bookmarks (
  message_id TEXT PRIMARY KEY,
  waddle_id TEXT NOT NULL,
  note TEXT,
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,

  INDEX idx_waddle_time (waddle_id, created_at DESC)
);

CREATE TABLE read_state (
  waddle_id TEXT NOT NULL,
  conversation_id TEXT NOT NULL,   -- or channel_id in traditional mode
  last_read_at DATETIME NOT NULL,
  last_message_id TEXT,

  PRIMARY KEY (waddle_id, conversation_id)
);
```

### Shared Views Schema (Waddle DB)

```sql
-- Per-Waddle DB
CREATE TABLE shared_views (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  description TEXT,
  icon TEXT,
  created_by TEXT NOT NULL,        -- user_id
  is_official BOOLEAN DEFAULT false, -- Created by admins
  sort_order TEXT DEFAULT 'newest',
  group_by TEXT DEFAULT 'conversation',
  usage_count INTEGER DEFAULT 0,   -- How many users use this view
  created_at DATETIME DEFAULT CURRENT_TIMESTAMP,

  INDEX idx_official (is_official)
);

CREATE TABLE shared_view_filters (
  id TEXT PRIMARY KEY,
  view_id TEXT NOT NULL,
  type TEXT NOT NULL,
  operator TEXT NOT NULL,
  value TEXT NOT NULL,

  FOREIGN KEY (view_id) REFERENCES shared_views(id) ON DELETE CASCADE
);
```

### View Query Engine

```typescript
// views-worker/query-engine.ts
export async function applyView(
  view: View,
  waddleId: string,
  userId: string,
  env: Env
): Promise<Message[]> {
  const waddleDb = await getWaddleDb(waddleId, env);

  // Build SQL query from view filters
  let query = 'SELECT * FROM messages WHERE 1=1';
  const params: any[] = [];

  for (const filter of view.filters) {
    switch (filter.type) {
      case 'tag':
        if (filter.operator === 'includes') {
          query += ` AND EXISTS (
            SELECT 1 FROM message_tags
            WHERE message_id = messages.id
            AND tag IN (${filter.value.map(() => '?').join(',')})
          )`;
          params.push(...filter.value);
        }
        break;

      case 'user':
        if (filter.operator === 'equals') {
          query += ' AND user_id = ?';
          params.push(filter.value);
        } else if (filter.operator === 'excludes') {
          query += ' AND user_id NOT IN (${filter.value.map(() => '?').join(',')})';
          params.push(...filter.value);
        }
        break;

      case 'content':
        if (filter.operator === 'matches') {
          query += ' AND content LIKE ?';
          params.push(`%${filter.value}%`);
        }
        break;

      case 'time':
        // Handle time-based filters
        break;
    }
  }

  // Apply sorting
  query += ` ORDER BY created_at ${view.sortOrder === 'oldest' ? 'ASC' : 'DESC'}`;

  // Execute query
  const result = await waddleDb
    .prepare(query)
    .bind(...params)
    .all();

  return result.results as Message[];
}
```

### User Database Provisioning

```typescript
export async function getUserDb(userId: string, env: Env): Promise<D1Database> {
  // Check if user DB exists
  const userDbName = `user_${userId.replace(/-/g, '')}`;

  // Try to get from env bindings
  if (env[userDbName]) {
    return env[userDbName];
  }

  // Provision new user DB
  const d1Response = await fetch(
    `https://api.cloudflare.com/client/v4/accounts/${env.ACCOUNT_ID}/d1/database`,
    {
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${env.CF_API_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ name: userDbName }),
    }
  );

  const { result: d1Database } = await d1Response.json();

  // Run user DB migrations
  await runUserDbMigrations(d1Database.id, env);

  // Store reference in central DB
  await env.CENTRAL_DB.prepare(`
    UPDATE users SET d1_database_id = ? WHERE id = ?
  `).bind(userDbName, userId).run();

  return env[userDbName];
}
```

## Migration Strategy

### Phase 1: Shared Views Only
- Start with shared views in Waddle DBs
- No per-user databases yet
- Validate view filtering logic

### Phase 2: Per-User Preferences
- Add user DB for preferences only
- Views still in Waddle DB
- Test provisioning flow

### Phase 3: Per-User Views
- Move views to user DBs
- Support view templates and copying
- Full personalization

### Phase 4: Advanced Features
- AI-suggested views
- View sharing and discovery
- View analytics

## Monitoring

- User DB provisioning time and success rate
- View query latency by complexity
- Most popular shared views
- Average views per user
- View creation and usage patterns

## References

- [D1 Best Practices](https://developers.cloudflare.com/d1/best-practices/)
- [Multi-tenant Database Design](https://martinfowler.com/articles/multi-tenant-saas.html)
- [View Pattern in Software Architecture](https://www.viewpattern.com/)