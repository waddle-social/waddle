# ADR-001: Cloudflare-Native Architecture

**Status:** Accepted

**Date:** 2025-09-30

## Context

Waddle is a social collaboration platform that needs to scale globally with low latency, support real-time features (chat, voice, video), and minimize operational complexity. We need to choose an infrastructure foundation that aligns with these requirements.

## Decision

We will build Waddle as a **Cloudflare-native application**, leveraging Cloudflare's edge computing platform as our primary infrastructure.

### Core Cloudflare Services

1. **Workers** - Serverless compute for all business logic
2. **D1** - SQLite-based databases for data persistence
3. **Durable Objects** - Stateful coordinators for real-time features
4. **R2** - Object storage for media assets
5. **RealTimeKit** - Voice and video infrastructure
6. **Containers** - GraphQL Federation router hosting
7. **Service Bindings** - Worker-to-worker communication
8. **Queues** - Event pub/sub system (TBD: may use alternative)

### Architecture Principles

- **Edge-first**: All compute runs at the edge, close to users
- **Serverless**: No server management, automatic scaling
- **Stateless workers, stateful DOs**: Workers are ephemeral, Durable Objects maintain state
- **Database per tenant**: Each Waddle and each user gets isolated D1 instances

## Consequences

### Positive

- **Global distribution**: Workers run in 300+ locations worldwide
- **Low latency**: Sub-50ms response times for most users
- **Auto-scaling**: No capacity planning required
- **Cost efficiency**: Pay only for actual usage
- **Simplified ops**: No infrastructure to manage
- **Integrated ecosystem**: All services work together seamlessly
- **Real-time native**: WebSocket support via Durable Objects
- **Self-hostable**: Users can deploy their own Waddle on Cloudflare

### Negative

- **Platform lock-in**: Tightly coupled to Cloudflare
- **Learning curve**: Team needs Cloudflare expertise
- **Resource limits**: Workers have 128MB memory, 30s CPU time
- **D1 limitations**: 10GB per database, eventual consistency for replication
- **Debugging complexity**: Distributed systems are harder to debug
- **Cost uncertainty**: Usage-based pricing can be unpredictable at scale

### Mitigation Strategies

- **Abstraction layer**: Use service interfaces to enable future portability
- **Monitoring**: Comprehensive observability from day one
- **Database sharding**: Plan for D1 limits with per-Waddle databases
- **Cost controls**: Set up billing alerts and usage quotas
- **Graceful degradation**: Design features to work with platform limits

## Alternatives Considered

### Traditional Cloud (AWS/GCP)

**Pros:** More control, familiar tooling, mature ecosystem
**Cons:** Complex ops, higher latency, more expensive, harder to scale globally

**Rejected because:** Operational complexity conflicts with our lean team goals.

### Vercel/Netlify Edge

**Pros:** Developer-friendly, good DX
**Cons:** Less control over data layer, limited stateful primitives, more expensive at scale

**Rejected because:** Insufficient real-time and stateful capabilities.

### Self-hosted Kubernetes

**Pros:** Full control, no lock-in
**Cons:** High operational burden, team expertise required, expensive

**Rejected because:** Team size doesn't support this operational complexity.

## Implementation Notes

- Start with single-region D1 databases, add replication later
- Use Cloudflare Analytics for initial monitoring
- Document platform-specific constraints for all features
- Create abstraction layers for critical dependencies
- Build deployment automation with Wrangler CLI

## References

- [Cloudflare Workers Documentation](https://developers.cloudflare.com/workers/)
- [Cloudflare D1 Limits](https://developers.cloudflare.com/d1/platform/limits/)
- [Durable Objects Guide](https://developers.cloudflare.com/durable-objects/)
- [Cloudflare Containers](https://developers.cloudflare.com/containers/)