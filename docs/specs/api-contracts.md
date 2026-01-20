# REST/HTTP API Specification

## Overview

This document specifies the HTTP API for Waddle Social, covering endpoints, authentication, rate limiting, and error handling.

## Base URL

```
Production: https://api.waddle.social/v1
Development: http://localhost:3000/v1
```

## Authentication

### Bearer Token

All authenticated requests require an Authorization header:

```
Authorization: Bearer <access_token>
```

### Token Types

| Type | Lifetime | Use Case |
|------|----------|----------|
| Access Token | 1 hour | API requests |
| Refresh Token | 30 days | Obtain new access tokens |
| Bot Token | Permanent | Bot authentication |

### OAuth Flow

See [Spec: ATProto Integration](./atproto-integration.md) for OAuth flow details.

## Request Format

### Headers

```
Content-Type: application/json
Authorization: Bearer <token>
X-Request-ID: <uuid>  (optional, for tracing)
```

### Query Parameters

- Pagination: `?page=1&limit=50`
- Filtering: `?status=active&type=text`
- Sorting: `?sort=created_at&order=desc`

### Request Body

JSON for POST/PUT/PATCH:

```json
{
  "name": "my-channel",
  "topic": "General discussion"
}
```

## Response Format

### Success Response

```json
{
  "data": { ... },
  "meta": {
    "request_id": "req_abc123"
  }
}
```

### Paginated Response

```json
{
  "data": [ ... ],
  "pagination": {
    "page": 1,
    "limit": 50,
    "total": 234,
    "has_more": true
  },
  "meta": {
    "request_id": "req_abc123"
  }
}
```

### Error Response

```json
{
  "error": {
    "code": "VALIDATION_ERROR",
    "message": "Invalid request body",
    "details": [
      {
        "field": "name",
        "message": "must be between 2 and 100 characters"
      }
    ]
  },
  "meta": {
    "request_id": "req_abc123"
  }
}
```

## Error Codes

### HTTP Status Codes

| Code | Meaning |
|------|---------|
| 200 | Success |
| 201 | Created |
| 204 | No Content |
| 400 | Bad Request |
| 401 | Unauthorized |
| 403 | Forbidden |
| 404 | Not Found |
| 409 | Conflict |
| 422 | Validation Error |
| 429 | Rate Limited |
| 500 | Server Error |

### Application Error Codes

| Code | Description |
|------|-------------|
| `VALIDATION_ERROR` | Request validation failed |
| `AUTHENTICATION_REQUIRED` | Missing or invalid token |
| `PERMISSION_DENIED` | Insufficient permissions |
| `RESOURCE_NOT_FOUND` | Requested resource doesn't exist |
| `RATE_LIMITED` | Too many requests |
| `CONFLICT` | Resource already exists |
| `INTERNAL_ERROR` | Server error |

## Rate Limiting

### Limits

| Endpoint Category | Rate | Window |
|-------------------|------|--------|
| Auth | 5 | 1 minute |
| Messages (write) | 5 | 5 seconds |
| Messages (read) | 50 | 1 minute |
| General API | 100 | 1 minute |
| Search | 30 | 1 minute |

### Headers

```
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 95
X-RateLimit-Reset: 1705320000
X-RateLimit-Bucket: general
```

### Rate Limit Response

```json
{
  "error": {
    "code": "RATE_LIMITED",
    "message": "Too many requests",
    "retry_after": 30
  }
}
```

## API Endpoints

### Authentication

```
POST   /auth/atproto/authorize    Start ATProto OAuth
GET    /auth/atproto/callback     OAuth callback
POST   /auth/refresh              Refresh access token
POST   /auth/logout               Revoke tokens
GET    /auth/@me                  Get current user
```

### Users

```
GET    /users/:did                Get user profile
PATCH  /users/@me                 Update own profile
GET    /users/@me/waddles         List joined Waddles
GET    /users/@me/dms             List DM conversations
```

### Waddles

```
POST   /waddles                   Create Waddle
GET    /waddles                   List user's Waddles
GET    /waddles/:id               Get Waddle
PATCH  /waddles/:id               Update Waddle
DELETE /waddles/:id               Delete Waddle
GET    /waddles/:id/channels      List channels
GET    /waddles/:id/members       List members
POST   /waddles/:id/members       Add member
DELETE /waddles/:id/members/:did  Remove member
GET    /waddles/:id/roles         List roles
POST   /waddles/:id/roles         Create role
GET    /waddles/:id/invites       List invites
POST   /waddles/:id/invites       Create invite
GET    /waddles/discover          Discover public Waddles
POST   /invites/:code/join        Join via invite
```

### Channels

```
POST   /waddles/:wid/channels     Create channel
GET    /channels/:id              Get channel
PATCH  /channels/:id              Update channel
DELETE /channels/:id              Delete channel
GET    /channels/:id/messages     Get messages
POST   /channels/:id/messages     Send message
GET    /channels/:id/pins         Get pinned messages
POST   /channels/:id/typing       Send typing indicator
```

### Messages

```
GET    /messages/:id              Get message
PATCH  /messages/:id              Edit message
DELETE /messages/:id              Delete message
PUT    /messages/:id/reactions/:emoji    Add reaction
DELETE /messages/:id/reactions/:emoji    Remove reaction
POST   /messages/:id/pin          Pin message
DELETE /messages/:id/pin          Unpin message
POST   /messages/:id/threads      Create thread
```

### Direct Messages

```
GET    /dms                       List DMs
POST   /dms                       Create DM
GET    /dms/:id                   Get DM
PATCH  /dms/:id                   Update DM
DELETE /dms/:id                   Leave/close DM
GET    /dms/:id/messages          Get messages
POST   /dms/:id/messages          Send message
```

### Files

```
POST   /uploads                   Get upload URL
GET    /uploads/:id               Get upload status
DELETE /uploads/:id               Cancel upload
```

### Search

```
GET    /search                    Global search
GET    /waddles/:id/search        Search in Waddle
GET    /channels/:id/search       Search in channel
```

### Presence

```
GET    /presence/@me              Get own presence
PATCH  /presence/@me              Update presence
GET    /waddles/:id/presence      Get Waddle presence
```

## Example Requests

### Create a Waddle

```http
POST /v1/waddles HTTP/1.1
Host: api.waddle.social
Authorization: Bearer eyJ...
Content-Type: application/json

{
  "name": "Penguin Enthusiasts",
  "description": "A community for penguin lovers",
  "visibility": "public"
}
```

Response:
```json
{
  "data": {
    "id": "waddle_abc123",
    "name": "Penguin Enthusiasts",
    "description": "A community for penguin lovers",
    "visibility": "public",
    "owner_did": "did:plc:xyz",
    "member_count": 1,
    "created_at": "2024-01-15T10:30:00.000Z"
  },
  "meta": {
    "request_id": "req_xyz789"
  }
}
```

### Send a Message

```http
POST /v1/channels/ch_general/messages HTTP/1.1
Host: api.waddle.social
Authorization: Bearer eyJ...
Content-Type: application/json

{
  "content": "Hello everyone! 🐧",
  "attachments": ["upload_abc123"]
}
```

### Get Message History

```http
GET /v1/channels/ch_general/messages?limit=50&before=msg_xyz HTTP/1.1
Host: api.waddle.social
Authorization: Bearer eyJ...
```

## Webhooks

### Outgoing Webhooks

For bot integration, configure webhook URLs to receive events:

```json
POST /your-webhook-url HTTP/1.1
Content-Type: application/json
X-Waddle-Signature: sha256=abc123...

{
  "type": "message.created",
  "timestamp": "2024-01-15T10:30:00.000Z",
  "data": { ... }
}
```

### Signature Verification

```
X-Waddle-Signature: sha256=<hex(HMAC-SHA256(secret, body))>
```

## Versioning

API version in URL path: `/v1/`, `/v2/`

- Breaking changes require new version
- Deprecation notice 6 months before removal
- `Sunset` header indicates deprecation date

## Related

- [ADR-0002: Axum Web Framework](../adrs/0002-axum-web-framework.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [Spec: XMPP Integration](./xmpp-integration.md)
- [Spec: ATProto Integration](./atproto-integration.md)
