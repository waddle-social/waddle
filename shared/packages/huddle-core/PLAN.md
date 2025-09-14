# Kernel - Shared Modules

## Purpose

The kernel contains shared functionality organized by business domain. Each module is self-contained with its own lexicon (if applicable), types, validation, and business logic.

## Architecture Principles

- **Functional Grouping**: Modules are organized by business function, not technical type
- **Self-Contained**: Each module includes everything needed for its domain
- **Type Safety**: Full TypeScript with generated types from lexicons
- **Pure Functions**: Prefer pure, testable functions with no side effects
- **Immutable Data**: Use immutable patterns throughout

## Module Structure

### slot-offer/
Office hours and availability windows that hosts publish to their PDS.

**Files:**
- `lexicon.json` - ATProto lexicon definition
- `types.ts` - Generated TypeScript types
- `validation.ts` - Zod schemas for runtime validation
- `helpers.ts` - Utility functions for slot manipulation

**Responsibilities:**
- Define the public slot offer format
- Validate slot offer data
- Transform between PDS and internal formats
- Policy tier management (mutual/follower/anyone)

### booking-request/
Guest requests for booking time with a host.

**Files:**
- `lexicon.json` - ATProto lexicon definition
- `types.ts` - Generated TypeScript types  
- `validation.ts` - Zod schemas
- `constraints.ts` - Constraint validation logic

**Responsibilities:**
- Define booking request format
- Validate guest constraints
- Check policy compliance

### booking/
Confirmed bookings and their state machine.

**Files:**
- `lexicon.json` - ATProto lexicon (minimal public fields)
- `types.ts` - Full booking types (public + private)
- `validation.ts` - Zod schemas
- `state-machine.ts` - Booking state transitions

**States:**
- `pending` - Initial request
- `held` - Tentative hold placed
- `confirmed` - Booking confirmed
- `canceled` - Booking canceled
- `rescheduled` - Booking rescheduled

### calendar/
Calendar operations and ICS generation.

**Files:**
- `ics.ts` - ICS file generation
- `availability.ts` - Free/busy calculations
- `types.ts` - Calendar-specific types
- `timezone.ts` - Timezone handling

**Responsibilities:**
- Generate ICS files for bookings
- Calculate availability windows
- Merge multiple calendar sources
- Handle timezone conversions

### auth/
Authentication and encryption utilities.

**Files:**
- `atproto.ts` - ATProto OAuth helpers
- `oauth.ts` - Generic OAuth utilities
- `encryption.ts` - Token encryption/decryption
- `types.ts` - Auth types and interfaces

**Responsibilities:**
- ATProto DID/handle resolution
- OAuth token management
- Secure token storage
- Session validation

### matching/
The core matching algorithm for finding optimal booking slots.

**Files:**
- `algorithm.ts` - Main matching logic
- `constraints.ts` - Constraint solver
- `scoring.ts` - Slot scoring functions
- `types.ts` - Matching types

**Algorithm:**
1. Fetch host's slot offers
2. Check guest's social graph relationship
3. Apply policy filters
4. Get real-time availability from calendars
5. Apply guest constraints
6. Score and rank candidates
7. Return optimal matches

### queue-messages/
Typed message definitions for Cloudflare Queues.

**Files:**
- `calendar-tasks.ts` - Calendar sync messages
- `notify-tasks.ts` - Notification messages
- `index-tasks.ts` - Firehose indexing messages
- `types.ts` - Common message types

**Message Types:**
- CalendarTask: `refresh-token`, `check-availability`, `create-event`, `place-hold`
- NotifyTask: `booking-confirmed`, `booking-canceled`, `reminder`
- IndexTask: `slot-offer`, `booking-request`, `record-delete`

### durable-objects/
Protocols and helpers for Durable Objects.

**Files:**
- `protocols.ts` - DO communication protocols
- `helpers.ts` - Common DO utilities
- `types.ts` - DO message types

**Protocols:**
- Request/response format
- Error handling
- State persistence patterns
- Alarm management

### rate-limiting/
Token bucket rate limiting implementation.

**Files:**
- `token-bucket.ts` - Core algorithm
- `types.ts` - Rate limit configurations
- `helpers.ts` - Utility functions

**Configuration:**
- Requests per minute/hour
- Burst capacity
- Per-user vs global limits

### observability/
Monitoring and observability setup.

**Files:**
- `otel.ts` - OpenTelemetry configuration
- `zpages.ts` - zPages implementation
- `metrics.ts` - Metric definitions
- `tracing.ts` - Distributed tracing

**Metrics:**
- Request latency
- Queue depth
- Match success rate
- Calendar API latency

### turnstile/
Cloudflare Turnstile verification.

**Files:**
- `verify.ts` - Server-side verification
- `types.ts` - Turnstile types

## Dependencies

```json
{
  "@atproto/api": "^0.x",
  "@atproto/lexicon": "^0.x",
  "zod": "^3.x",
  "@opentelemetry/api": "^1.x",
  "ical-generator": "^6.x"
}
```

## Testing Strategy

Each module should have:
- Unit tests for pure functions
- Integration tests for external APIs
- Property-based tests for algorithms
- Snapshot tests for lexicon compatibility

## Usage Examples

```typescript
// Import specific modules
import { SlotOffer, validateSlotOffer } from '@huddle/kernel/slot-offer'
import { generateICS } from '@huddle/kernel/calendar'
import { matchSlots } from '@huddle/kernel/matching'

// Use typed queue messages
import { CalendarTask } from '@huddle/kernel/queue-messages'

const task: CalendarTask = {
  type: 'check-availability',
  userId: 'did:plc:...',
  timeRange: { start: ..., end: ... }
}
```

## Development Workflow

1. Define lexicon (if applicable)
2. Generate TypeScript types
3. Write validation schemas
4. Implement business logic
5. Add comprehensive tests
6. Document public API

## Module Guidelines

- Keep modules focused on a single domain
- Export clean, typed public APIs
- Hide implementation details
- Use dependency injection for external services
- Maintain backwards compatibility