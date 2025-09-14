# Firehose Ingest Worker

## Purpose

Subscribe to the Bluesky firehose, filter for Huddle-specific lexicons, and index records into D1 for discovery and matching.

## Architecture

- **Connection**: WebSocket to bsky.network firehose
- **Filtering**: Process only com.huddle.* records
- **Indexing**: Queue tasks for D1 upserts
- **Resilience**: Auto-reconnect with cursor tracking

## Migrations

### 0001_firehose_cursor.sql
```sql
CREATE TABLE firehose_cursor (
  id TEXT PRIMARY KEY DEFAULT 'main',
  cursor TEXT NOT NULL,
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

INSERT INTO firehose_cursor (cursor) VALUES ('0');
```

## Core Components

### index.ts
Main worker entry point.
- Maintains WebSocket connection
- Handles reconnection logic
- Routes messages to consumer

### consumer.ts
Processes firehose messages.
- Decodes CAR files
- Filters for relevant records
- Validates lexicon format
- Produces to index queue

### filters.ts
Record filtering logic.
- Collection name matching
- DID filtering (if needed)
- Record type validation

### indexer.ts
Queue consumer for index tasks.
- Processes index-tasks queue
- Upserts to D1 tables
- Handles deletions

## Firehose Protocol

### Message Types
- `com.atproto.sync.subscribeRepos#commit` - Record changes
- `com.atproto.sync.subscribeRepos#handle` - Handle updates
- `com.atproto.sync.subscribeRepos#tombstone` - Deletions

### Processing Flow
1. Connect to wss://bsky.network/xrpc/com.atproto.sync.subscribeRepos
2. Resume from last cursor if available
3. For each message:
   - Decode CAR file
   - Extract record operations
   - Filter for com.huddle.* collections
   - Queue for indexing
4. Update cursor periodically

## Records to Process

### com.huddle.slotOffer
- Index in offers_index table
- Track host availability windows
- Update on modifications
- Remove on deletion

### com.huddle.bookingRequest
- Track active requests
- Monitor for status changes
- Clean up completed requests

### com.huddle.booking
- Public booking records
- Status tracking
- Statistics gathering

## Queue Messages

```typescript
interface IndexTask {
  type: 'upsert' | 'delete'
  collection: string
  uri: string
  did: string
  rkey: string
  record?: unknown
  timestamp: number
}
```

## Bindings

```toml
# wrangler.toml
name = "huddle-firehose"
main = "src/index.ts"
compatibility_date = "2025-09-14"

[[d1_databases]]
binding = "DB"
database_name = "huddle-db"

[[queues.producers]]
binding = "Q_INDEX"
queue = "index-tasks"

[[queues.consumers]]
queue = "index-tasks"
max_batch_size = 100
max_retries = 3
```

## Error Handling

- **Connection Errors**: Exponential backoff reconnection
- **Malformed Records**: Log and skip
- **Queue Failures**: Use DLQ after retries
- **Cursor Updates**: Batch updates every 100 records

## Performance

- Process messages in batches
- Use cursor checkpointing
- Implement backpressure when queue is full
- Monitor lag between firehose and processing

## Monitoring

Key metrics:
- Messages per second
- Processing lag
- Filter hit rate
- Queue depth
- Reconnection count

## Resilience

### Reconnection Strategy
```typescript
let backoff = 1000 // Start with 1 second
const maxBackoff = 60000 // Max 1 minute

while (true) {
  try {
    await connectToFirehose()
  } catch (error) {
    await sleep(backoff)
    backoff = Math.min(backoff * 2, maxBackoff)
  }
}
```

### Cursor Management
- Update cursor every 100 messages
- Store in D1 for persistence
- Resume from cursor on restart

## Development

```bash
# Install dependencies
bun install

# Run migrations
wrangler d1 migrations apply DB

# Start dev server
wrangler dev

# Test with mock firehose
bun test

# Deploy
wrangler deploy
```

## Testing

- Mock WebSocket server for unit tests
- Test record filtering logic
- Verify cursor persistence
- Load test with high message volume
- Test reconnection scenarios

## Future Enhancements

- Add metrics for specific lexicon types
- Implement selective reprocessing
- Add admin commands for cursor manipulation
- Support multiple firehose sources
- Add data validation and sanitization