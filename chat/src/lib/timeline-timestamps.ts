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
//   0. Pending self-sends (deliveryStatus ∈ {queued, sending, failed})
//      always sort *after* delivered/peer messages, regardless of
//      `createdAt`. Optimistic rows carry client-wall-clock timestamps
//      and a backwards-drifted client clock could otherwise slot them
//      into the middle of the timeline; reconciliation in
//      `mergeLiveMessage` swaps the optimistic row for the
//      server-stamped copy and a normal sort then places it correctly.
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
  // synced (non-pending) bucket.
  return status === "queued" || status === "sending" || status === "failed";
}
