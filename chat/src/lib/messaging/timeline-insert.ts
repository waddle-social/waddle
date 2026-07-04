import type { TimelineMessage } from "@/lib/chat-ui";
import { mergeMessageIds } from "@/lib/message-ids";
import { compareTimelineMessages, pickAuthoritativeTimestamp } from "@/lib/timeline-timestamps";
import { retractTimelineMessage } from "@/lib/messaging/retraction";
import { consumeReconciledEchoIds, findLiveMergeTarget } from "@/lib/messaging/self-echo";

// Timeline insertion mechanics shared by the channel and DM pipelines:
// ordering, id-based dedupe, in-place merge of duplicate arrivals, and
// the MAM duplicate/tombstone merge. The only divergence is the channel's
// post-insert forum-context pass (divergence 7), injected via
// `LiveInsertPolicy.finalize`.

export interface LiveInsertPolicy {
  /** Divergence 7: the channel re-derives forum context after every insert. */
  finalize?: (messages: TimelineMessage[]) => TimelineMessage[];
}

export interface LiveInsertResult {
  messages: TimelineMessage[];
  /** `false` when the message reconciled into an existing row instead. */
  appended: boolean;
}

/**
 * In-place merge of a duplicate arrival into its existing row: merges
 * XEP-0359 id aliases, keeps the higher-authority `createdAt` (so a
 * redelivered live stanza with a `fallback` stamp can't overwrite a true
 * server stamp that landed via MAM first), mirrors the incoming
 * `linkPreviews` own-property wholesale, and promotes a reconciled
 * self-echo to "delivered" (the echo is authoritative; it supersedes any
 * prior "sending" / "failed" optimistic state).
 */
function mergedLiveRow(existing: TimelineMessage, incoming: TimelineMessage): TimelineMessage {
  const mergedIds = mergeMessageIds(existing, incoming.id, incoming.wireIds);
  const authoritativeTimestamp = pickAuthoritativeTimestamp(
    { createdAt: existing.createdAt, createdAtSource: existing.createdAtSource },
    { createdAt: incoming.createdAt, createdAtSource: incoming.createdAtSource },
  );
  const updated: TimelineMessage = {
    ...existing,
    ...incoming,
    id: mergedIds.id,
    createdAt: authoritativeTimestamp.createdAt,
    createdAtSource: authoritativeTimestamp.createdAtSource,
  };
  if (!Object.prototype.hasOwnProperty.call(incoming, "linkPreviews")) {
    delete updated.linkPreviews;
  }
  if (mergedIds.wireIds?.length) updated.wireIds = mergedIds.wireIds;
  else delete updated.wireIds;
  if (existing.isSelf && incoming.isSelf) {
    updated.deliveryStatus = "delivered";
  }
  return updated;
}

/**
 * Merges a regular incoming live message (no retract, no correction)
 * into the timeline. If the message reconciles an existing row (by id,
 * alias, or self-echo body fallback) it merges in place and consumes the
 * pending optimistic ids; otherwise it appends in timestamp order.
 */
export function insertLiveMessage(
  messages: TimelineMessage[],
  msg: TimelineMessage,
  pendingEchoClientIds: Set<string>,
  policy: LiveInsertPolicy = {},
): LiveInsertResult {
  const finalize = policy.finalize ?? ((timeline: TimelineMessage[]) => timeline);
  const existing = findLiveMergeTarget(messages, msg, pendingEchoClientIds);
  if (existing) {
    const merged = messages
      .map((m) => (m.id === existing.id ? mergedLiveRow(m, msg) : m))
      .sort(compareTimelineMessages);
    consumeReconciledEchoIds(pendingEchoClientIds, existing);
    return { messages: finalize(merged), appended: false };
  }
  return {
    messages: finalize([...messages, msg].sort(compareTimelineMessages)),
    appended: true,
  };
}

function mergeAuthoritativeLinkPreviews(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): TimelineMessage {
  if (Object.prototype.hasOwnProperty.call(incoming, "linkPreviews")) {
    return { ...existing, linkPreviews: incoming.linkPreviews };
  }
  if (!existing.linkPreviews) return existing;
  const next = { ...existing };
  delete next.linkPreviews;
  return next;
}

export interface ArchiveMergeHooks {
  /** DM-only: adopt the archive row's call-thread payload. */
  mergeExtras?: (result: TimelineMessage, incoming: TimelineMessage) => TimelineMessage;
}

/**
 * MAM duplicate merge: applies an incoming archive hit onto the row we
 * already had. Tombstones the row when the hit is a retraction, upgrades
 * the timestamp if the archive carries a higher-authority stamp (the
 * path that corrects a previously-fallback live insert once the archive
 * result lands), and mirrors the authoritative `linkPreviews` set.
 */
export function mergeRetractionTombstone(
  existing: TimelineMessage,
  incoming: TimelineMessage,
  hooks: ArchiveMergeHooks = {},
): TimelineMessage {
  let result = incoming.isRetracted
    ? retractTimelineMessage(existing, incoming.retractionId)
    : existing;
  const authoritativeTimestamp = pickAuthoritativeTimestamp(
    { createdAt: result.createdAt, createdAtSource: result.createdAtSource },
    { createdAt: incoming.createdAt, createdAtSource: incoming.createdAtSource },
  );
  if (
    authoritativeTimestamp.createdAt !== result.createdAt
    || authoritativeTimestamp.createdAtSource !== result.createdAtSource
  ) {
    result = {
      ...result,
      createdAt: authoritativeTimestamp.createdAt,
      createdAtSource: authoritativeTimestamp.createdAtSource,
    };
  }
  if (incoming.isRetracted) return result;
  if (hooks.mergeExtras) result = hooks.mergeExtras(result, incoming);
  return mergeAuthoritativeLinkPreviews(result, incoming);
}
