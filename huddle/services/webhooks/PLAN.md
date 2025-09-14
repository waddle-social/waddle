# Webhooks Worker

## Purpose

Receive and process webhooks from calendar providers (Google, Microsoft) for real-time updates on calendar changes.

## Architecture

- **Webhook Endpoints**: Provider-specific handlers
- **Signature Verification**: Validate webhook authenticity
- **Event Processing**: Queue calendar updates
- **Deduplication**: Prevent duplicate processing

## Migrations

### 0001_webhook_events.sql
```sql
CREATE TABLE webhook_events (
  id TEXT PRIMARY KEY,
  provider TEXT NOT NULL CHECK (provider IN ('google','microsoft')),
  connector_id TEXT REFERENCES connectors(id),
  event_type TEXT NOT NULL,
  resource_id TEXT,
  change_type TEXT,
  raw_payload TEXT NOT NULL,
  processed_at INTEGER,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX idx_webhook_events_unprocessed 
  ON webhook_events(processed_at) 
  WHERE processed_at IS NULL;

CREATE TABLE webhook_subscriptions (
  id TEXT PRIMARY KEY,
  connector_id TEXT NOT NULL REFERENCES connectors(id),
  provider TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  expiration INTEGER NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
```

## Core Components

### index.ts
Main worker entry point.

```typescript
app.post('/webhooks/google', handleGoogleWebhook)
app.post('/webhooks/microsoft', handleMicrosoftWebhook)
app.get('/healthz', handleHealth)
```

### providers/google.ts

#### Webhook Types
- `sync` - Initial sync message
- `exists` - Resource exists
- `not_exists` - Resource deleted
- `update` - Resource changed

#### Verification
```typescript
function verifyGoogleWebhook(request: Request): boolean {
  // Verify X-Goog-Channel-Token
  // Verify X-Goog-Resource-State
  // Check channel expiration
}
```

#### Processing
1. Verify webhook signature
2. Parse notification type
3. Extract resource details
4. Queue calendar sync task
5. Return 200 immediately

### providers/microsoft.ts

#### Webhook Types
- `created` - New event created
- `updated` - Event modified
- `deleted` - Event removed

#### Validation
```typescript
function verifyMicrosoftWebhook(request: Request): boolean {
  // Validate JWT token
  // Verify tenant ID
  // Check subscription expiration
}
```

#### Processing
1. Validate webhook token
2. Handle validation requests
3. Process change notifications
4. Queue appropriate tasks
5. Return 202 Accepted

### verification.ts
Common verification utilities.

- HMAC signature verification
- JWT validation
- Timestamp validation
- IP allowlist checking

## Webhook Registration

### Google Calendar
```typescript
interface GoogleSubscription {
  id: string
  type: 'web_hook'
  address: string  // https://huddle.waddle.social/webhooks/google
  token: string    // Shared secret
  expiration: number
}
```

### Microsoft Graph
```typescript
interface MicrosoftSubscription {
  changeType: 'created,updated,deleted'
  notificationUrl: string  // https://huddle.waddle.social/webhooks/microsoft
  resource: string         // /me/events
  expirationDateTime: string
  clientState: string      // Shared secret
}
```

## Queue Messages

Produced to calendar-tasks queue:

```typescript
interface WebhookCalendarTask {
  type: 'webhook-sync'
  connectorId: string
  provider: 'google' | 'microsoft'
  changeType: string
  resourceId?: string
  timestamp: number
}
```

## Bindings

```toml
# wrangler.toml
name = "huddle-webhooks"
main = "src/index.ts"
compatibility_date = "2025-09-14"

[vars]
GOOGLE_WEBHOOK_TOKEN = "..."
MICROSOFT_CLIENT_STATE = "..."

[[d1_databases]]
binding = "DB"
database_name = "huddle-db"

[[queues.producers]]
binding = "Q_CALENDAR"
queue = "calendar-tasks"

[[kv_namespaces]]
binding = "KV_DEDUPE"
id = "kv_dedupe_id"
```

## Security

### Verification Requirements
- Always verify webhook signatures/tokens
- Validate source IP addresses
- Check webhook subscription validity
- Implement replay protection
- Rate limit by source

### Deduplication
- Use KV to track processed event IDs
- 1-hour TTL on dedupe keys
- Handle duplicate notifications gracefully

## Error Handling

- **Invalid Signature**: Return 401, log attempt
- **Unknown Subscription**: Return 404
- **Processing Error**: Return 500, queue for retry
- **Rate Limited**: Return 429
- **Expired Subscription**: Return 410, clean up

## Performance

- Return response immediately (< 3 seconds)
- Process asynchronously via queues
- Use KV for deduplication
- Batch related events
- Implement circuit breakers

## Monitoring

Key metrics:
- Webhook receipt rate
- Verification failures
- Processing latency
- Queue depth
- Duplicate rate

## Subscription Management

### Registration Flow
1. User connects calendar
2. Register webhook with provider
3. Store subscription details
4. Handle verification callback
5. Start receiving events

### Renewal
- Track expiration dates
- Renew 24 hours before expiry
- Handle renewal failures gracefully
- Notify user if renewal fails

## Testing

```bash
# Unit tests
bun test

# Webhook simulation
bun test:webhooks

# Load testing
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

# Test webhook locally
ngrok http 8787

# Deploy
wrangler deploy
```

## Debugging

### Webhook Testing Tools
- Google: Push Notification Debugger
- Microsoft: Graph Explorer
- Local: ngrok + request bin

### Common Issues
- Clock skew causing signature failures
- Subscription expiration not renewed
- Duplicate notifications from provider
- Network timeouts on response

## Future Enhancements

- Support for other calendar providers
- Batch notification processing
- Webhook replay for debugging
- Advanced filtering rules
- Real-time sync status updates