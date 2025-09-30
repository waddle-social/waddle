# ADR-002: GraphQL Federation Architecture

**Status:** Accepted

**Date:** 2025-09-30

## Context

Waddle consists of multiple independent features (chat, calendaring, integrations, views, etc.), each potentially developed and deployed independently. We need an API architecture that supports:

- Independent feature development and deployment
- Type-safe client-server communication
- Schema composition across services
- Real-time subscriptions
- Efficient querying with minimal over-fetching

## Decision

We will use **GraphQL Federation** with the following architecture:

### Components

1. **Federation Router** - Single GraphQL gateway deployed on Cloudflare Containers
2. **Feature Workers** - Independent Cloudflare Workers, each implementing a GraphQL subgraph
3. **Schema Registry** - Workers publish SDL schemas to router on deployment
4. **Per-Service Tech Stack** - Pothos (schema), graphql-yoga (server), Drizzle (ORM)

### Architecture

```
┌─────────────────────────────────────────┐
│  Clients (Web, Mobile, CLI)             │
└──────────────┬──────────────────────────┘
               │ GraphQL Queries/Mutations/Subscriptions
               ▼
┌─────────────────────────────────────────┐
│  Federation Router                       │
│  (Cloudflare Container)                  │
│  - Query planning                        │
│  - Schema composition                    │
│  - Response merging                      │
└──┬────────┬────────┬────────┬───────────┘
   │        │        │        │
   │        │        │        │ Service Bindings
   ▼        ▼        ▼        ▼
┌─────┐  ┌─────┐  ┌─────┐  ┌─────┐
│Chat │  │Views│  │Integ│  │Apps │  ... Feature Workers
│     │  │     │  │     │  │     │
│ D1  │  │ D1  │  │ D1  │  │ D1  │  Per-feature databases
└─────┘  └─────┘  └─────┘  └─────┘
```

### Schema Composition

Each feature worker:
- Defines its own GraphQL schema using **Pothos**
- Serves GraphQL via **graphql-yoga**
- Publishes endpoint and SDL to router on deployment
- Extends shared types (e.g., `User`, `Waddle`) as needed

Example:
```typescript
// chat-worker/schema.ts
import { builder } from './builder';

builder.queryType({
  fields: (t) => ({
    messages: t.field({
      type: [Message],
      args: { waddleId: t.arg.string(), limit: t.arg.int() },
      resolve: (parent, args, ctx) => getMessages(args),
    }),
  }),
});

// Extend User type from identity service
builder.objectRef<User>('User').implement({
  fields: (t) => ({
    recentMessages: t.field({
      type: [Message],
      resolve: (user) => getRecentMessages(user.id),
    }),
  }),
});
```

### Service Communication

- **Primary**: GraphQL Federation (router -> workers)
- **Inter-worker**: Cloudflare Service Bindings (when needed)
- **Events**: Pub/Sub system for async operations

## Consequences

### Positive

- **Independent deployment**: Features ship without coordinating releases
- **Type safety**: End-to-end TypeScript types from schema
- **Developer experience**: Pothos provides excellent schema-first DX
- **Scalability**: Each feature scales independently
- **Clear boundaries**: Domain-driven service separation
- **Schema evolution**: Backward-compatible changes without downtime
- **Efficient queries**: Clients request exactly what they need
- **Real-time support**: GraphQL subscriptions via Durable Objects

### Negative

- **Complexity**: Federation adds operational overhead
- **Query planning overhead**: Router must plan and merge queries
- **Schema coordination**: Must coordinate shared types across services
- **Debugging difficulty**: Distributed query tracing required
- **N+1 query risk**: Poor query planning can cause performance issues
- **Router as SPOF**: Federation router becomes critical dependency

### Mitigation Strategies

- **Schema linting**: Automated checks for breaking changes
- **Query complexity limits**: Prevent expensive queries
- **Distributed tracing**: OpenTelemetry for request flows
- **DataLoader pattern**: Batch and cache data fetching
- **Router redundancy**: Multiple router instances with load balancing
- **Schema versioning**: Explicit schema versioning strategy

## Alternatives Considered

### REST APIs

**Pros:** Simple, well-understood, easy to debug
**Cons:** Over-fetching, versioning complexity, no real-time, multiple round-trips

**Rejected because:** GraphQL provides better DX and client efficiency.

### gRPC

**Pros:** Efficient, strongly typed, good for service-to-service
**Cons:** Poor browser support, steeper learning curve, limited tooling

**Rejected because:** Browser support is critical for our web clients.

### Monolithic GraphQL API

**Pros:** Simpler to start, no federation complexity
**Cons:** Cannot deploy features independently, scaling challenges

**Rejected because:** Independent feature deployment is a core requirement.

### tRPC

**Pros:** End-to-end type safety, simpler than GraphQL, no schema
**Cons:** TypeScript-only, no federation story, limited real-time

**Rejected because:** Lack of language-agnostic schema limits future clients.

## Implementation Plan

### Phase 1: Foundation (Week 1-2)
- Deploy basic federation router on Cloudflare Containers
- Create Pothos/graphql-yoga template for feature workers
- Implement schema registration mechanism
- Build shared types (User, Waddle, Message)

### Phase 2: Core Features (Week 3-6)
- Migrate Colony (auth) to federation
- Build Chat worker with full schema
- Add Views worker
- Implement service bindings between workers

### Phase 3: Subscriptions (Week 7-8)
- Add GraphQL subscriptions via Durable Objects
- Real-time message delivery
- Presence updates

### Phase 4: Optimization (Week 9-10)
- Add DataLoader for batching
- Implement query complexity analysis
- Set up distributed tracing
- Performance testing and tuning

## Technical Details

### Router Configuration

```typescript
// federation-router/index.ts
import { ApolloGateway } from '@apollo/gateway';
import { ApolloServer } from '@apollo/server';

const gateway = new ApolloGateway({
  serviceList: [
    { name: 'colony', url: env.COLONY_WORKER_URL },
    { name: 'chat', url: env.CHAT_WORKER_URL },
    { name: 'views', url: env.VIEWS_WORKER_URL },
    // ... registered on deployment
  ],
  buildService: ({ url }) => {
    return new CloudflareServiceDataSource({ url, env });
  },
});

const server = new ApolloServer({ gateway });
```

### Feature Worker Template

```typescript
// feature-worker/index.ts
import { createYoga } from 'graphql-yoga';
import { schema } from './schema';

export default {
  async fetch(request: Request, env: Env) {
    const yoga = createYoga({
      schema,
      context: { env, request },
      graphiql: env.ENVIRONMENT === 'development',
    });

    return yoga.fetch(request, env);
  },
};
```

## References

- [Apollo Federation Specification](https://www.apollographql.com/docs/federation/)
- [Pothos GraphQL](https://pothos-graphql.dev/)
- [GraphQL Yoga](https://the-guild.dev/graphql/yoga-server)
- [Cloudflare Service Bindings](https://developers.cloudflare.com/workers/runtime-apis/bindings/service-bindings/)