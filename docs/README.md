# Waddle Documentation

Welcome to Waddle's technical and product documentation. This directory contains Architecture Decision Records (ADRs), Request for Comments (RFCs), and Product Requirements Documents (PRDs) that define Waddle's design and roadmap.

## What is Waddle?

Waddle is a next-generation social collaboration platform designed for developer communities, hobby groups, and modern online communities. Built Cloudflare-native with atproto identity, Waddle reimagines chat by removing rigid channels in favor of flexible, AI-powered conversation organization.

### Core Differentiators

- **Channel-less Chat**: Messages organized by conversations, not fixed channels
- **ATProto Identity**: Decentralized identity—one person, multiple interests
- **Web Integration**: RSS, YouTube, GitHub content flows directly into conversations
- **Personalized Views**: Each user creates custom filters for their workflow
- **Cloudflare Native**: Edge-first architecture with global distribution

### Target Communities

- **Developer Communities**: Open source projects, tech groups
- **Hobby Communities**: Board games, sci-fi fans, interest groups
- **Educational**: Rawkode Academy, online courses
- **Professional**: Remote teams, consultancies

## Architecture

Waddle uses a modern, distributed architecture:

```
┌────────────────────────────────────────────────┐
│  Clients (Web, Mobile)                         │
└──────────────┬─────────────────────────────────┘
               │ GraphQL Queries/Mutations/Subscriptions
               ▼
┌────────────────────────────────────────────────┐
│  GraphQL Federation Router                     │
│  (Cloudflare Container)                        │
└──┬───────┬────────┬────────┬────────┬──────────┘
   │       │        │        │        │
   │       │        │        │        │ Service Bindings
   ▼       ▼        ▼        ▼        ▼
┌──────┐┌──────┐┌──────┐┌──────┐┌──────┐
│Colony││Waddle││Views ││Integ ││ AI   │ Feature Workers
│(Auth)││(Chat)││      ││      ││      │
└──┬───┘└──┬───┘└──┬───┘└──┬───┘└──┬───┘
   │       │       │       │       │
   ▼       ▼       ▼       ▼       ▼
┌──────┐┌──────┐┌──────┐┌──────┐┌──────┐
│  D1  ││  D1  ││  D1  ││  D1  ││  D1  │ Per-feature databases
└──────┘└──────┘└──────┘└──────┘└──────┘

           ┌──────────────────┐
           │ Event Pub/Sub    │
           └──────────────────┘

┌────────────────────────────────────────────────┐
│  Durable Objects (Real-time coordination)      │
│  - WebSocket connections                       │
│  - Presence tracking                           │
│  - Voice/video sessions                        │
└────────────────────────────────────────────────┘

┌────────────────────────────────────────────────┐
│  Per-Waddle D1 Databases                       │
│  - Messages, conversations, roles              │
└────────────────────────────────────────────────┘

┌────────────────────────────────────────────────┐
│  Per-User D1 Databases                         │
│  - Views, preferences, bookmarks               │
└────────────────────────────────────────────────┘
```

### Key Technologies

- **Runtime**: Cloudflare Workers (edge compute)
- **Database**: D1 (SQLite at the edge)
- **Real-time**: Durable Objects + WebSockets
- **API**: GraphQL Federation (Pothos + graphql-yoga)
- **Identity**: AT Protocol (DPoP OAuth)
- **Voice/Video**: Cloudflare RealTimeKit
- **Storage**: R2 (object storage)
- **Container**: Cloudflare Containers (federation router)

## Documentation Structure

### Architecture Decision Records (ADRs)

ADRs document significant architectural decisions and their rationale.

- **[ADR-001: Cloudflare-Native Architecture](./adr/001-cloudflare-native-architecture.md)**
  - Why Cloudflare Workers, D1, Durable Objects
  - Trade-offs vs. traditional cloud
  - Migration path and constraints

- **[ADR-002: GraphQL Federation Architecture](./adr/002-graphql-federation-architecture.md)**
  - Feature workers with independent schemas
  - Federation router composition
  - Service-to-service communication

- **[ADR-003: Per-Waddle Database Isolation](./adr/003-per-waddle-database-isolation.md)**
  - One D1 database per Waddle
  - Scalability and data isolation
  - Provisioning and routing

- **[ADR-004: Per-User View Storage](./adr/004-per-user-view-storage.md)**
  - One D1 database per user for preferences
  - View definitions and filters
  - Privacy and personalization

- **[ADR-005: Event-Driven Architecture](./adr/005-event-driven-architecture.md)**
  - Pub/sub event bus
  - Domain events for async communication
  - Event schema and versioning

- **[ADR-006: AT Protocol Identity Layer](./adr/006-atproto-identity-layer.md)**
  - DIDs for decentralized identity
  - DPoP OAuth implementation
  - Colony authentication service

- **[ADR-007: Durable Objects for Real-Time](./adr/007-durable-objects-for-realtime.md)**
  - WebSocket state management
  - Presence and typing indicators
  - Voice/video coordination

### Request for Comments (RFCs)

RFCs provide detailed technical specifications for major features.

- **[RFC-001: Channel-less Conversation Model](./rfc/001-channelless-conversation-model.md)**
  - How messages become conversations
  - AI and human tagging
  - Conversation grouping algorithm
  - Data model and queries

- **[RFC-002: Web Integration System](./rfc/002-web-integration-system.md)**
  - RSS, YouTube, GitHub integrations
  - Scheduled polling architecture
  - Webhook handling
  - Message formatting

- **[RFC-003: GraphQL Schema Design](./rfc/003-graphql-schema-design.md)**
  - Federated schema structure
  - Pothos implementation patterns
  - Queries, mutations, subscriptions
  - Type extensions across services

### Product Requirements Documents (PRDs)

PRDs define product features from a user perspective.

- **[PRD-001: Channel-less Chat Experience](./prd/001-channelless-chat-experience.md)**
  - User stories and personas
  - Message posting flow
  - Conversation discovery
  - Success metrics

- **[PRD-002: Web Content Integration](./prd/002-web-content-integration.md)**
  - Integration setup UX
  - Supported platforms (RSS, YouTube, GitHub)
  - Content formatting
  - Management dashboard

- **[PRD-003: Personalized Views](./prd/003-personalized-views.md)**
  - View creation and management
  - Filter types and combinations
  - Shared views for admins
  - Discovery and onboarding

### Data Services

Federated workers are generated from a shared projen template—use these resources when delivering new services or iterating on existing ones:

- **Generator Playbook**: [`generators/projen-data-service/README.md`](../generators/projen-data-service/README.md) — template options, generated layout, and synthesis commands.
- **Reference Implementation**: [`waddle/services/waddle`](../waddle/services/waddle) — baseline service output; see its README for regenerate/migrate/test flows.
- **Topics Service Quickstart**: [`docs/prd/topics-service-quickstart.md`](./prd/topics-service-quickstart.md) — end-to-end walkthrough (generate, bind D1, migrate, test, publish schema).

When proposing or updating a data service, ensure specs capture generator inputs (`generate.ts`), schema snapshot expectations, and CI scripts (`bun run test:<service>`). Updates MUST refresh schema snapshots and quickstart instructions alongside code changes.

## Key Features

### 1. Channel-less Conversations

Traditional platforms force communities into rigid channel structures. Waddle organizes messages by conversations—logical groupings formed dynamically through AI analysis and human hashtags.

**Benefits:**
- No channel management overhead
- Conversations span topics naturally
- Users see what they want via views
- New members aren't overwhelmed

**Technical:** See RFC-001, PRD-001

### 2. AT Protocol Identity

Users authenticate with their atproto DID (Decentralized Identifier), often from Bluesky. This provides:

**Benefits:**
- User owns their identity
- Works across any atproto service
- No password management
- Portable social graph (future)

**Technical:** See ADR-006

### 3. Web Integrations

External content flows directly into Waddle:

- **RSS Feeds**: Blog posts, news
- **YouTube**: New videos, livestreams
- **GitHub**: Releases, PRs, issues

**Benefits:**
- Automatic content sharing
- Rich previews and embeds
- Centralized discussion
- No manual cross-posting

**Technical:** See RFC-002, PRD-002

### 4. Personalized Views

Each user creates custom views filtering messages by:

- Tags (e.g., #help, #kubernetes)
- Users (e.g., my posts, mentions)
- Content (keywords, regex)
- Time (last 24 hours, etc.)

**Benefits:**
- Workflow optimization
- Role-based filtering
- Multi-interest support
- Efficient catch-up

**Technical:** See ADR-004, PRD-003

### 5. Real-Time Features

- **WebSocket connections** via Durable Objects
- **Presence tracking** (online/offline/idle)
- **Typing indicators** per conversation
- **Voice/video** with RealTimeKit

**Technical:** See ADR-007

## Development Roadmap

### Phase 1: Foundation (Weeks 1-8)
- ✅ Colony authentication (atproto + DPoP)
- GraphQL Federation router setup
- Basic message posting (no channels)
- Waddle creation and membership
- Real-time message delivery

### Phase 2: Conversations (Weeks 9-16)
- AI message tagging
- Conversation grouping
- Tag-based filtering
- Basic views (All, My Messages)

### Phase 3: Views & Integrations (Weeks 17-24)
- Custom view creation
- Shared admin views
- RSS integration
- YouTube integration
- GitHub integration

### Phase 4: Polish & Scale (Weeks 25-32)
- Advanced filters (regex, time ranges)
- View templates
- Performance optimization
- Mobile apps
- Voice/video (RealTimeKit)

### Phase 5: Advanced Features (Future)
- AI conversation summaries
- Threaded discussions
- Apps within Waddles (kanban, wikis, calendars)
- Federation with other atproto services
- Self-hosting toolkit

## Contributing

### For Developers

1. Read relevant ADRs for architectural context
2. Check RFCs for technical specifications
3. Follow patterns in existing workers
4. Use Pothos for GraphQL schemas
5. Write tests for all features

### For Product

1. Read PRDs for user context
2. Propose new features via PRs to docs
3. Validate designs against metrics
4. Interview users regularly

### For Design

1. Review PRD user flows
2. Create prototypes matching specs
3. Test with target personas
4. Document patterns in design system

## Getting Help

- **Technical Questions**: Check ADRs and RFCs first
- **Product Questions**: Review PRDs
- **Implementation Help**: See working code in respective workers
- **Architecture Decisions**: Propose new ADR via PR

## License

See root LICENSE file.

## References

- [Cloudflare Workers](https://developers.cloudflare.com/workers/)
- [AT Protocol](https://atproto.com/)
- [GraphQL Federation](https://www.apollographql.com/docs/federation/)
- [Pothos GraphQL](https://pothos-graphql.dev/)
- [Durable Objects](https://developers.cloudflare.com/durable-objects/)

---

**Last Updated:** 2025-10-28
**Version:** 1.1.0
