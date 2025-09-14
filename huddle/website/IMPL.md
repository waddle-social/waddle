# Huddle Implementation Order

This document outlines the implementation order and dependencies for building Huddle - an ATProto-based calendar scheduling application on Cloudflare infrastructure.

## Overview

Huddle is an AppView that:
- Reads public "office hours" from users' PDSes
- Stores private calendar data in Cloudflare (D1, KV, R2, DOs)
- Automatically matches availability between hosts and guests
- Integrates with Google Calendar and Microsoft Outlook

## Implementation Phases

### Phase 0: Foundation (Week 1)
**Goal**: Set up shared code and infrastructure basics

1. **kernel/** - Shared modules grouped by function
   - [ ] `auth/` - ATProto and OAuth utilities
   - [ ] `calendar/` - ICS generation and availability logic
   - [ ] `observability/` - OTel and zpages setup
   - [ ] Base types and validation

2. **infrastructure/** - Cloudflare resource setup
   - [ ] D1 databases (huddle-db, huddle-db-dev)
   - [ ] KV namespaces (features, graph, oauth)
   - [ ] R2 buckets (huddle-files)
   - [ ] Queues (calendar-tasks, notify-tasks, index-tasks)

### Phase 1: Core Infrastructure (Week 2-3)
**Goal**: Basic worker and data layer

3. **services/appview/**
   - [ ] Basic worker setup with Hono
   - [ ] D1 migrations (users, connectors, offers_index, bookings, idempotency)
   - [ ] Health endpoint
   - [ ] Durable Object skeletons

4. **kernel/slot-offer**, **kernel/booking-request**, **kernel/booking**
   - [ ] ATProto lexicon definitions
   - [ ] TypeScript types
   - [ ] Validation schemas

### Phase 2: Authentication (Week 4)
**Goal**: User authentication and session management

5. **kernel/auth/**
   - [ ] ATProto OAuth flow implementation
   - [ ] Token encryption/decryption
   - [ ] Session management

6. **websites/huddle/**
   - [ ] Basic Astro site setup
   - [ ] Auth pages (login, callback)
   - [ ] Protected route middleware

### Phase 3: ATProto Integration (Week 5)
**Goal**: Connect to Bluesky firehose and implement XRPC

7. **services/firehose/**
   - [ ] WebSocket connection to bsky.network
   - [ ] Record filtering for huddle lexicons
   - [ ] Migration for cursor tracking
   - [ ] Queue producer for indexing

8. **kernel/queue-messages/**
   - [ ] Calendar task types
   - [ ] Notify task types
   - [ ] Index task types

9. **services/appview/routes/**
   - [ ] XRPC endpoint implementations
   - [ ] com.huddle.match
   - [ ] com.huddle.finalize
   - [ ] com.huddle.listOffers

### Phase 4: Calendar Integration (Week 6-7)
**Goal**: Connect to Google/Microsoft calendars

10. **services/calendar-sync/**
    - [ ] OAuth setup for providers
    - [ ] Migrations for sync state
    - [ ] Google Calendar client
    - [ ] Microsoft Graph client
    - [ ] Free/busy queries
    - [ ] Event creation

11. **kernel/calendar/**
    - [ ] ICS generation
    - [ ] Availability calculations
    - [ ] Timezone handling

12. **services/appview/durable/user-connector.ts**
    - [ ] Encrypted token storage
    - [ ] Provider API calls
    - [ ] Free/busy caching

### Phase 5: Matching Logic (Week 8-9)
**Goal**: Implement the core matching algorithm

13. **kernel/matching/**
    - [ ] Constraint solver
    - [ ] Policy enforcement (mutual/follower/anyone)
    - [ ] Optimal slot selection

14. **services/appview/durable/host-match.ts**
    - [ ] Candidate generation
    - [ ] Atomic hold placement
    - [ ] Graph-based filtering

15. **services/appview/durable/booking.ts**
    - [ ] State machine (pending → held → confirmed)
    - [ ] Hold expiry logic
    - [ ] Confirmation flow

### Phase 6: Webhooks & Notifications (Week 10)
**Goal**: Handle provider webhooks and send notifications

16. **services/webhooks/**
    - [ ] Google webhook handler
    - [ ] Microsoft webhook handler
    - [ ] Signature verification
    - [ ] Queue integration

17. **services/notify/**
    - [ ] Email channel (ICS attachments)
    - [ ] Calendar invites
    - [ ] ATProto DM integration (future)
    - [ ] Template system

### Phase 7: User Interface (Week 11)
**Goal**: Complete user-facing features

18. **websites/huddle/dashboard/**
    - [ ] Office hours management
    - [ ] Calendar connector setup
    - [ ] Booking list view
    - [ ] Policy configuration

19. **websites/huddle/book/**
    - [ ] Host discovery
    - [ ] Booking request form
    - [ ] Status tracking

20. **kernel/rate-limiting/**
    - [ ] Token bucket implementation
    - [ ] DO-based rate limiter

### Phase 8: Admin & Operations (Week 12)
**Goal**: Admin tools and observability

21. **websites/colony/**
    - [ ] Admin dashboard
    - [ ] User management
    - [ ] Metrics visualization
    - [ ] Queue monitoring

22. **kernel/observability/**
    - [ ] Complete OTel setup
    - [ ] zPages implementation
    - [ ] WAE integration

23. **Cron Jobs** (across services)
    - [ ] OAuth token refresh
    - [ ] Hold expiry sweeps
    - [ ] Drift reindexing

## Dependencies Graph

```
kernel/auth → everything
kernel/calendar → services/appview, services/notify
kernel/matching → services/appview/durable/host-match
kernel/queue-messages → all services
kernel/observability → all services

services/firehose → kernel/queue-messages
services/appview → kernel/* (all modules)
services/calendar-sync → kernel/auth, kernel/calendar
services/webhooks → kernel/queue-messages
services/notify → kernel/calendar, kernel/queue-messages

websites/huddle → services/appview (via API)
websites/colony → all services (monitoring)
```

## Critical Path

The minimum viable path to a working booking:

1. kernel/auth (authentication)
2. services/appview (basic worker)
3. kernel/slot-offer (data model)
4. services/calendar-sync (availability)
5. kernel/matching (algorithm)
6. services/appview/durable/* (state management)
7. websites/huddle (UI)

## Development Guidelines

### Each Service Should:
- Have its own migrations in `/migrations`
- Export OTel metrics and zpages
- Use typed messages from kernel/queue-messages
- Handle errors gracefully with retries
- Log to Workers Analytics Engine

### Testing Strategy:
- Unit tests for kernel modules
- Integration tests for each service
- E2E tests for critical user flows
- Use Miniflare for local development

### Deployment:
- Each service deploys independently
- Use `wrangler deploy` per service
- Environment: development (Miniflare) → production

## Next Steps

1. Start with Phase 0: Foundation
2. Set up kernel modules with types and validation
3. Initialize Cloudflare resources
4. Build incrementally, testing each phase
5. Maintain backwards compatibility for lexicons

## Notes

- TypeScript and Bun only (no Node.js/npm)
- Functional grouping in kernel (not by type)
- Services are independently deployable
- All private data stays in Cloudflare
- Public data (office hours) on users' PDSes
- Automated matching only (no manual slot selection)