import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import {
  findSenderScopedIdTarget,
  hasMessageSenderContinuity,
} from "@/lib/messaging/sender-scoped-ids";

// Self-echo reconciliation for the live merge path, shared verbatim by
// the channel and DM pipelines (XEP-0359 alias resolution + body-match
// fallback). Both sides must stay behaviourally identical here so a
// fresh-session resume can't retarget the wrong row when an echo arrives
// without a XEP-0359 alias.

/**
 * Body-match fallback is a last resort: only when the incoming message
 * is a self-send that carries no XEP-0359 wire aliases (so no alias
 * path could have already reconciled it to an existing row).
 *
 * Per-candidate scoping in `findLiveMergeTarget` is what keeps this safe:
 *   - the pending path requires `pendingEchoClientIds.has(existing.id)`
 *     — only locally-issued optimistic ids are ever in that set;
 *   - the preserved path requires `existing.deliveryStatus` to be
 *     non-`delivered` (i.e. queued/sending/failed) — so a previously
 *     reconciled-delivered row can never be retargeted by a same-body
 *     replay.
 */
function canUseSelfEchoBodyFallback(msg: TimelineMessage): boolean {
  return msg.isSelf && (msg.wireIds?.length ?? 0) === 0;
}

/**
 * Locates the existing timeline row an incoming live message reconciles
 * into: first by primary id / XEP-0359 wire aliases, then — for
 * alias-less self-echoes only — by the pending-echo body match, then by
 * the preserved (untracked, non-delivered) self-row body match.
 */
export function findLiveMergeTarget(
  messages: TimelineMessage[],
  msg: TimelineMessage,
  pendingEchoClientIds: ReadonlySet<string>,
): TimelineMessage | undefined {
  const mucScoped = !!msg.authorOccupantJid || messages.some((message) => !!message.authorOccupantJid);
  const existingById = mucScoped
    ? findSenderScopedIdTarget(messages, msg)
    : [msg.id, ...(msg.wireIds ?? [])]
      .map((id) => findMessageById(messages, id))
      .find((message): message is TimelineMessage => !!message);
  const bodyFallbackAllowed = !existingById && canUseSelfEchoBodyFallback(msg);
  const pendingSelfEcho = bodyFallbackAllowed
    ? messages.find(
      (m) =>
        pendingEchoClientIds.has(m.id)
        && m.isSelf
        && m.body === msg.body
        && (!mucScoped || hasMessageSenderContinuity(m, msg)),
    )
    : undefined;
  const preservedSelfEcho = bodyFallbackAllowed
    ? [...messages].reverse().find(
      (m) =>
        m.isSelf
        && m.body === msg.body
        && !!m.deliveryStatus
        && m.deliveryStatus !== "delivered"
        && (!mucScoped || hasMessageSenderContinuity(m, msg)),
    )
    : undefined;
  return existingById ?? pendingSelfEcho ?? preservedSelfEcho;
}

/**
 * Drops every pending optimistic id a reconciliation accounted for —
 * both the previous primary id and any pre-existing wire aliases — so a
 * later same-body replay within the fallback window can never retarget
 * the now-delivered row.
 */
export function consumeReconciledEchoIds(
  pendingEchoClientIds: Set<string>,
  existing: TimelineMessage,
): void {
  pendingEchoClientIds.delete(existing.id);
  for (const alias of existing.wireIds ?? []) pendingEchoClientIds.delete(alias);
}
