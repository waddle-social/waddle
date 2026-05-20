export function compareTimelineTimestamps(
  left: string | null | undefined,
  right: string | null | undefined,
): number {
  const leftValue = typeof left === "string" ? left : "";
  const rightValue = typeof right === "string" ? right : "";
  const leftMs = Date.parse(leftValue);
  const rightMs = Date.parse(rightValue);
  if (Number.isFinite(leftMs) && Number.isFinite(rightMs)) {
    return leftMs === rightMs ? 0 : leftMs < rightMs ? -1 : 1;
  }
  return leftValue.localeCompare(rightValue);
}

// Provenance of a message's `createdAt` value. The same logical message
// can flow through multiple producers (live socket, MAM catch-up,
// queued outbound, stream-resume redelivery) and each producer can
// supply a different timestamp with a different degree of authority.
// We tag the source so merges can pick the most trustworthy stamp
// instead of letting "whoever wrote last wins" reorder the timeline
// on tab restore (see PR1 plan).
//
// Authority ranking (highest first):
//   * archive: XEP-0313 MAM forwarded `<delay/>` — the server's own
//     copy of the message in its archive. Canonical.
//   * delay:   XEP-0203 `<delay/>` on a live stanza — added by the
//     server when the message was buffered/redelivered. Also server-
//     authoritative for the original send time.
//   * queued:  client wall-clock captured at the moment the user
//     pressed send. Only used for self-sent rows awaiting echo.
//   * fallback: client wall-clock at decode of a live stanza that
//     arrived without any `<delay/>`. This is correct for genuinely-
//     new arrivals ("now" really is the right answer) but is wrong
//     for redelivered stanzas (XEP-0198 resume, carbons that lose
//     delay, server replays). Lowest authority — any other source
//     replaces it on merge.
export type TimestampSource = "archive" | "delay" | "queued" | "fallback";

const TIMESTAMP_SOURCE_RANK: Record<TimestampSource, number> = {
  archive: 3,
  delay: 2,
  queued: 1,
  fallback: 0,
};

interface DatedTimelineFields {
  createdAt: string;
  createdAtSource: TimestampSource;
}

// Pick the more authoritative of two stamps for the *same* logical
// message. On equal authority, prefer the earlier wall-clock value:
// for `archive`/`delay` the server stamp is deterministic so equal
// authority implies equal value; for `queued`/`fallback` the earlier
// stamp is the one closer to when the user actually performed the
// action.
//
// An unknown / undefined source ranks below every named source — test
// fixtures sometimes build `TimelineMessage` via `as TimelineMessage`
// casts that omit the field, and we never want such a row to suppress
// a properly-tagged authoritative stamp on merge.
export function pickAuthoritativeTimestamp(
  existing: DatedTimelineFields,
  incoming: DatedTimelineFields,
): DatedTimelineFields {
  const existingRank = TIMESTAMP_SOURCE_RANK[existing.createdAtSource] ?? -1;
  const incomingRank = TIMESTAMP_SOURCE_RANK[incoming.createdAtSource] ?? -1;
  if (incomingRank > existingRank) return incoming;
  if (existingRank > incomingRank) return existing;
  return compareTimelineTimestamps(existing.createdAt, incoming.createdAt) <= 0
    ? existing
    : incoming;
}

// `compareTimelineMessages` must be a *total* order over distinct
// messages: never return 0 for two messages with different identities.
// Burst messages share `createdAt` to the millisecond, optimistic echoes
// reuse a client-generated timestamp before the server stamps the
// canonical copy, and tab-restore can mix server-clock and client-clock
// stamps in the same array. Without a tiebreaker, the visible order of
// equal-timestamp messages depends on which producer's microtask
// happened to run first — i.e. it changes across reloads.
//
// Ordering rules:
//   0. Pending self-sends (deliveryStatus ∈ {queued, sending}) always
//      sort *after* delivered/peer messages, regardless of `createdAt`.
//      Optimistic rows carry client-wall-clock timestamps and a
//      backwards-drifted client clock could otherwise slot them into
//      the middle of the timeline; reconciliation in `mergeLiveMessage`
//      swaps the optimistic row for the server-stamped copy and a
//      normal sort then places it correctly.
//
//      `failed` is intentionally *not* in this bucket — it's a terminal
//      status and the row keeps the position the user expects to retry
//      or delete it from, alongside whatever was being sent at that
//      moment.
//   1. `createdAt` (server time when present).
//   2. `stanzaId` — XEP-0359 unique stable IDs, lexicographically
//      monotonic per archive (the canonical wire identity assigned by
//      the server / room).
//   3. `id` — falls back to the message's primary identifier so the
//      comparator stays total even for optimistic rows that have not
//      yet acquired a stanza-id.
export function compareTimelineMessages(
  left: { createdAt?: string; stanzaId?: string; id?: string; deliveryStatus?: string },
  right: { createdAt?: string; stanzaId?: string; id?: string; deliveryStatus?: string },
): number {
  const leftPending = isPendingDelivery(left.deliveryStatus);
  const rightPending = isPendingDelivery(right.deliveryStatus);
  if (leftPending !== rightPending) return leftPending ? 1 : -1;
  const timestamp = compareTimelineTimestamps(left.createdAt, right.createdAt);
  if (timestamp !== 0) return timestamp;
  const stanza = (left.stanzaId ?? "").localeCompare(right.stanzaId ?? "");
  if (stanza !== 0) return stanza;
  return (left.id ?? "").localeCompare(right.id ?? "");
}

function isPendingDelivery(status: string | undefined): boolean {
  // `deliveryStatus` is only set for self-sent rows. Peer messages and
  // delivered self-sends leave it `undefined` — treat those as the
  // synced (non-pending) bucket. `failed` is a terminal state; keep
  // the row at its attempt position so the user can retry / delete it
  // in context.
  return status === "queued" || status === "sending";
}
