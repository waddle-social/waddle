import type { TimelineMessage } from "@/lib/chat-ui";

// Body-match self-echo reconciliation gates. Both the DM and channel
// `mergeLiveMessage` paths must stay behaviourally identical here so a
// fresh-session resume can't retarget the wrong row when an echo arrives
// without a XEP-0359 alias. Centralised so future tweaks only land in
// one place.

/**
 * Body-match fallback is a last resort: only when the incoming message
 * is a self-send that carries no XEP-0359 wire aliases (so no alias
 * path could have already reconciled it to an existing row).
 *
 * Per-candidate scoping at the call site is what keeps this safe:
 *   - the pending path requires `pendingEchoClientIds.has(existing.id)`
 *     — only locally-issued optimistic ids are ever in that set;
 *   - the preserved path requires `existing.deliveryStatus` to be
 *     non-`delivered` (i.e. queued/sending/failed) — so a previously
 *     reconciled-delivered row can never be retargeted by a same-body
 *     replay.
 */
export function canUseSelfEchoBodyFallback(msg: TimelineMessage): boolean {
  return msg.isSelf && (msg.wireIds?.length ?? 0) === 0;
}
