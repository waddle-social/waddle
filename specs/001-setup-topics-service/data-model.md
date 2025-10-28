# Data Model

## Entities

### Topic
- `id` (text, uuid primary key) — uniquely identifies a topic record.
- `title` (text, not null, <= 120 chars) — human-readable label surfaced in clients.
- `description` (text, nullable) — optional supporting context.
- `scope` (text enum: `global`, `owner`, `waddle`, not null) — clarifies which filter fields are populated.
- `ownerId` (text, nullable) — Cloudflare user identifier; required when `scope === "owner"`.
- `waddleSlug` (text, nullable, FK → `waddle.services.waddle.data-model.waddle.slug`) — populated when `scope === "waddle"`.
- `createdAt` (integer, default current timestamp) — milliseconds since epoch.
- `updatedAt` (integer, default current timestamp, auto-updated via trigger) — milliseconds since epoch for optimistic concurrency.

## Relationships

- `Topic.waddleSlug` references `waddle/services/waddle` `waddle.slug` for integrity when the topic is scoped to a waddle.
- Owner linkage relies on federation context (user ID) rather than a local FK because user identities are managed by a separate service.

## Indexes & Constraints

- Primary key on `id` (UUID v4).
- Unique partial index on (`ownerId`, `title`) filtered by `scope = 'owner'` to prevent duplicates in a personal context.
- Unique partial index on (`waddleSlug`, `title`) filtered by `scope = 'waddle'` to avoid duplicates inside a waddle.
- Check constraints enforcing:
  - `ownerId IS NOT NULL` when `scope = 'owner'`.
  - `waddleSlug IS NOT NULL` when `scope = 'waddle'`.
  - Only one of `ownerId` or `waddleSlug` is populated for non-global topics.

## Validation Rules

- Title must be trimmed and non-empty; enforce max length via zod schema to align with UI expectations.
- Description optional but limited to 512 characters to keep payload small.
- Queries must respect caller authorization: a user can only access topics where:
  - `scope = 'global'`, or
  - `scope = 'owner'` and `ownerId` matches caller, or
  - `scope = 'waddle'` and caller has membership granted by upstream resolver.

## State & Lifecycle

- Read-only service: topics are created/updated elsewhere once the dedicated write service exists.
- `updatedAt` enables future auditing and ordering without writes in this iteration.
