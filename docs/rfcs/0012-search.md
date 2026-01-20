# RFC-0012: Full-Text Search

## Summary

Full-text search enables users to find messages, channels, users, and content across Waddles they have access to.

## Motivation

Users need to:
- Find past conversations and decisions
- Locate specific messages by content
- Search within channels or across Waddles
- Filter by author, date, type

## Detailed Design

### Search Scope

Users can search:
- **Messages**: In channels they can read
- **Channels**: By name/topic in Waddles they're in
- **Users**: By handle or display name
- **Files**: By filename in accessible channels

### Search Index

```
SearchDocument
├── id: String
├── type: "message" | "channel" | "user" | "file"
├── content: String (searchable text)
├── waddle_id: UUID (for scoping)
├── channel_id: UUID (optional)
├── author_did: DID (optional)
├── created_at: Timestamp
├── embedding: Vec<f32> (for semantic search)
└── metadata: SearchMetadata
```

### Search Query

```
SearchQuery
├── query: String
├── scope: SearchScope
├── filters: SearchFilters
├── sort: SortOrder
├── page: Integer
└── page_size: Integer

SearchScope
├── type: "all" | "messages" | "channels" | "users" | "files"
├── waddle_ids: UUID[] (optional, limit to specific)
└── channel_ids: UUID[] (optional)

SearchFilters
├── author: DID (optional)
├── before: Timestamp (optional)
├── after: Timestamp (optional)
├── has_attachment: Boolean (optional)
├── is_pinned: Boolean (optional)
└── mentions_me: Boolean (optional)
```

### Search Modes

**1. Keyword Search** (default):
- Full-text matching with stemming
- Boolean operators: AND, OR, NOT
- Phrase matching: `"exact phrase"`
- Wildcard: `test*`

**2. Semantic Search** (AI-powered):
- Query embedded to vector
- Cosine similarity against message embeddings
- Better for conceptual queries
- See [RFC-0007: AI Features](./0007-ai-integrations.md)

**3. Hybrid Search**:
- Combines keyword and semantic
- Reciprocal rank fusion for scoring

### Search Result

```
SearchResult
├── total: Integer
├── page: Integer
├── results: SearchHit[]
└── facets: SearchFacets (optional)

SearchHit
├── id: String
├── type: String
├── score: Float
├── highlight: Map<String, String[]>
└── document: SearchDocument
```

### Indexing Pipeline

1. **Message created** → event emitted
2. **Indexer receives** event
3. **Content extracted** (text, attachments)
4. **Embedding generated** (if semantic enabled)
5. **Document indexed** in search engine

### Ephemeral Content

- Ephemeral messages excluded from search by default
- Removed from index when TTL expires
- Configurable per channel

### Permission Enforcement

Search respects access control:
- Only returns content user can access
- Post-filter applied to results
- Waddle/channel membership checked

### Implementation Options

**Search backend options**:
- **SQLite FTS5**: Built into libSQL, simple deployment
- **Meilisearch**: Fast, easy, good for moderate scale
- **Elasticsearch**: Powerful, but heavy
- **Tantivy**: Rust-native, embeddable

Recommended: **SQLite FTS5** for MVP, migrate to Meilisearch if needed.

### Search Limits

| Resource | Limit |
|----------|-------|
| Query length | 500 characters |
| Results per page | 50 |
| Max result depth | 10,000 |
| Queries per minute | 30 |

## API Endpoints

```
GET    /search                    Global search
GET    /waddles/:id/search        Search within Waddle
GET    /channels/:id/search       Search within channel
GET    /search/messages           Message-specific search
GET    /search/users              User search
GET    /search/files              File search
```

### Query Examples

```
# Find messages from alice about deployment
GET /search?q=deployment&scope=messages&author=did:plc:alice

# Find pinned messages in a channel
GET /channels/:id/search?q=*&is_pinned=true

# Semantic search for "how to set up auth"
GET /search?q=how+to+set+up+auth&mode=semantic
```

## Response Format

```json
{
  "total": 42,
  "page": 1,
  "results": [
    {
      "id": "msg_123",
      "type": "message",
      "score": 0.95,
      "highlight": {
        "content": ["...the <em>deployment</em> process..."]
      },
      "document": {
        "content": "Let's discuss the deployment process for v2.0",
        "author_did": "did:plc:alice",
        "channel_id": "...",
        "created_at": "2024-01-15T10:30:00Z"
      }
    }
  ]
}
```

## Related

- [RFC-0004: Rich Message Format](./0004-message-format.md)
- [RFC-0005: Ephemeral Content](./0005-ephemeral-content.md)
- [RFC-0007: AI Features](./0007-ai-integrations.md)
- [ADR-0004: Turso/libSQL](../adrs/0004-turso-libsql-database.md)
