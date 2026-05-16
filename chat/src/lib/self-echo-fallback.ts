import type { TimelineMessage } from "@/lib/chat-ui";

// Body-match self-echo reconciliation gates. Both the DM and channel
// `mergeLiveMessage` paths must stay behaviourally identical here so a
// fresh-session resume can't retarget a stale optimistic / preserved-failed
// send when the wire id has already been reconciled through a XEP-0359
// alias path. Centralised so future tweaks only land in one place.

const CLIENT_GENERATED_MESSAGE_ID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

/**
 * Cap for the time delta between an optimistic insert's `sentAt` and an
 * incoming echo's `createdAt` when reconciling via body match. Sized to
 * comfortably exceed the XEP-0198 resume window so a reconnect-replayed
 * echo is still eligible, while rejecting unrelated history replays.
 */
const SELF_ECHO_FALLBACK_MAX_DELTA_MS = 5 * 60 * 1000;

function isLikelyClientGeneratedMessageId(id: string): boolean {
  return CLIENT_GENERATED_MESSAGE_ID_PATTERN.test(id);
}

/**
 * Body-match fallback is a last resort: only when the incoming message
 * is a self-send whose primary id is still client-generated and carries
 * no wire aliases (so no XEP-0359 alias path could have reconciled it).
 */
export function canUseSelfEchoBodyFallback(msg: TimelineMessage): boolean {
  return msg.isSelf
    && (msg.wireIds?.length ?? 0) === 0
    && isLikelyClientGeneratedMessageId(msg.id);
}

/**
 * Bound the body-match fallback to a small time window so a same-body
 * MAM replay (after a fresh-session reload) cannot absorb a long-stale
 * optimistic / preserved-failed entry.
 */
export function withinSelfEchoFallbackWindow(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): boolean {
  const existingTime = Date.parse(existing.createdAt);
  const incomingTime = Date.parse(incoming.createdAt);
  if (!Number.isFinite(existingTime) || !Number.isFinite(incomingTime)) return false;
  return Math.abs(incomingTime - existingTime) <= SELF_ECHO_FALLBACK_MAX_DELTA_MS;
}
