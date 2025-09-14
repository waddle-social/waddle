# Calendar Sync Worker

## Purpose

Manage calendar provider integrations, OAuth flows, and calendar API interactions for Google Calendar and Microsoft Outlook.

## Architecture

- **OAuth Management**: Token exchange, refresh, and encryption
- **Provider Abstraction**: Common interface for multiple providers
- **Queue Processing**: Async calendar operations
- **Rate Limiting**: Provider-specific throttling

## Migrations

### 0001_sync_state.sql
```sql
CREATE TABLE sync_state (
  connector_id TEXT PRIMARY KEY REFERENCES connectors(id),
  last_sync_cursor TEXT,
  sync_status TEXT CHECK (sync_status IN ('idle','syncing','error')),
  last_error TEXT,
  retry_count INTEGER DEFAULT 0,
  next_retry_at INTEGER,
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE oauth_state (
  state TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  provider TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  scopes TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  expires_at INTEGER NOT NULL
);
```

## Core Components

### index.ts
Worker entry point and queue consumer.

```typescript
export default {
  async fetch(request: Request, env: Env) {
    // OAuth callback endpoints
  },
  async queue(batch: MessageBatch<CalendarTask>, env: Env) {
    // Process calendar tasks
  }
}
```

### providers/google/

#### client.ts
Google Calendar API client.
- Calendar.Events API
- Calendar.FreeBusy API
- Batch requests
- Exponential backoff

#### oauth.ts
Google OAuth 2.0 implementation.
- Authorization URL generation
- Token exchange
- Token refresh
- Scope management

#### freebusy.ts
Free/busy query implementation.
- Batch calendar queries
- Time zone handling
- Result aggregation

### providers/microsoft/

#### client.ts
Microsoft Graph API client.
- Calendar endpoints
- Batch API support
- Delta sync
- Throttling handling

#### oauth.ts
Microsoft OAuth 2.0 implementation.
- MSAL integration
- Token caching
- Refresh logic
- Tenant handling

#### freebusy.ts
Outlook availability implementation.
- FindMeetingTimes API
- ScheduleInformation endpoint
- Working hours support

### queue-consumer.ts
Processes calendar-tasks queue.

```typescript
interface CalendarTask {
  type: 'refresh-token' | 'check-availability' | 'create-event' | 'place-hold' | 'cancel-hold'
  connectorId: string
  payload: any
  retryCount?: number
}
```

## OAuth Flows

### Google OAuth
1. Generate authorization URL with PKCE
2. User authorizes and returns with code
3. Exchange code for tokens
4. Encrypt refresh token and store in D1
5. Set up refresh schedule

### Microsoft OAuth
1. Generate authorization URL
2. Handle organizational consent
3. Exchange code for tokens
4. Store encrypted tokens
5. Handle tenant-specific flows

## API Operations

### Check Availability
```typescript
interface AvailabilityRequest {
  connectorId: string
  timeMin: string
  timeMax: string
  timeZone: string
  calendars?: string[]
}

interface AvailabilityResponse {
  busy: Array<{start: string, end: string}>
  workingHours?: Array<{day: string, start: string, end: string}>
}
```

### Create Event
```typescript
interface CreateEventRequest {
  connectorId: string
  summary: string
  description?: string
  start: string
  end: string
  timeZone: string
  attendees: Array<{email: string}>
  location?: string
}
```

### Place Hold
```typescript
interface PlaceHoldRequest {
  connectorId: string
  start: string
  end: string
  timeZone: string
  holdId: string
}
```

## Bindings

```toml
# wrangler.toml
name = "huddle-calendar-sync"
main = "src/index.ts"
compatibility_date = "2025-09-14"

[vars]
GOOGLE_CLIENT_ID = "..."
GOOGLE_CLIENT_SECRET = "..."
MICROSOFT_CLIENT_ID = "..."
MICROSOFT_CLIENT_SECRET = "..."
OAUTH_ENC_KEY = "..."

[[d1_databases]]
binding = "DB"
database_name = "huddle-db"

[[kv_namespaces]]
binding = "KV_OAUTH"
id = "kv_oauth_id"

[[queues.consumers]]
queue = "calendar-tasks"
max_batch_size = 10
max_retries = 5

[[queues.producers]]
binding = "Q_NOTIFY"
queue = "notify-tasks"
```

## Rate Limiting

### Google Calendar
- 500 queries per 100 seconds per user
- 50,000 queries per day per project
- Use exponential backoff on 429

### Microsoft Graph
- 10,000 requests per 10 minutes per app
- 4 concurrent requests per user
- Respect Retry-After header

## Error Handling

- **401 Unauthorized**: Refresh token and retry
- **403 Forbidden**: Check scopes, notify user
- **429 Too Many Requests**: Exponential backoff
- **503 Service Unavailable**: Retry with backoff
- **Network Errors**: Queue for retry

## Security

- Encrypt refresh tokens with AES-256-GCM
- Store only encrypted tokens in D1
- Use PKCE for OAuth flows
- Validate redirect URIs
- Implement token rotation

## Performance

- Cache access tokens in KV (until expiry)
- Batch API requests where possible
- Use delta sync for incremental updates
- Implement request coalescing
- Pre-fetch busy times for active users

## Monitoring

Key metrics:
- OAuth success rate
- Token refresh failures
- API latency by provider
- Rate limit hits
- Queue processing time

## Testing

```bash
# Unit tests
bun test

# Integration tests with mock providers
bun test:integration

# Load tests
bun test:load
```

## Development

```bash
# Install dependencies
bun install

# Run migrations
wrangler d1 migrations apply DB

# Start dev server
wrangler dev

# Deploy
wrangler deploy
```

## Future Enhancements

- CalDAV support for other providers
- Webhook subscriptions for real-time updates
- Calendar sync for offline support
- Multi-calendar aggregation
- Recurring event support