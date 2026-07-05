import type { TimelineMessage } from "@/lib/chat-ui";
import { mergeMessageIds } from "@/lib/message-ids";
import { compareTimelineMessages, pickAuthoritativeTimestamp } from "@/lib/timeline-timestamps";
import { retractTimelineMessage } from "@/lib/messaging/retraction";
import { consumeReconciledEchoIds, findLiveMergeTarget } from "@/lib/messaging/self-echo";
import { findSynthesizedIdMergeTarget } from "@/lib/messaging/synthesized-id-merge";

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
  // #1182: a fabricated primary id must never displace a real one — keep
  // the existing row's identity and demote the incoming UUID to an alias.
  const mergedIds = incoming.synthesizedId
    ? mergeMessageIds(existing, existing.id, [incoming.id, ...(incoming.wireIds ?? [])])
    : mergeMessageIds(existing, incoming.id, incoming.wireIds);
  const authoritativeTimestamp = pickAuthoritativeTimestamp(
    { createdAt: existing.createdAt, createdAtSource: existing.createdAtSource },
    { createdAt: incoming.createdAt, createdAtSource: incoming.createdAtSource },
  );
  let updated: TimelineMessage = {
    ...existing,
    ...incoming,
    id: mergedIds.id,
    createdAt: authoritativeTimestamp.createdAt,
    createdAtSource: authoritativeTimestamp.createdAtSource,
  };
  if (!Object.prototype.hasOwnProperty.call(incoming, "linkPreviews")) {
    delete updated.linkPreviews;
  }
  // XEP-0308: a redelivered copy of the pre-correction original (SM
  // replay, or the #675 bootstrap re-insert racing a MAM page that
  // already applied the correction) must not roll the row's content
  // back — the applied correction stays authoritative.
  if (existing.isEdited && !incoming.isEdited) {
    updated.body = existing.body;
    updated.isEdited = true;
    if (existing.markup) updated.markup = existing.markup;
    else delete updated.markup;
    if (existing.references) updated.references = existing.references;
    else delete updated.references;
    if (Object.prototype.hasOwnProperty.call(existing, "linkPreviews")) {
      updated.linkPreviews = existing.linkPreviews;
    } else {
      delete updated.linkPreviews;
    }
    if (existing.extensionAnnotations) {
      updated.extensionAnnotations = existing.extensionAnnotations;
      if (existing.extensionBodyFallback) updated.extensionBodyFallback = true;
      else delete updated.extensionBodyFallback;
    } else {
      delete updated.extensionAnnotations;
      delete updated.extensionBodyFallback;
    }
  }
  // XEP-0424: a tombstone is terminal. If either side is retracted (a
  // MAM-applied tombstone meeting a redelivered pre-retraction original
  // via SM replay or the #675 bootstrap re-insert, or vice versa), the
  // merged row stays a tombstone with every content-bearing field
  // stripped.
  if (existing.isRetracted || incoming.isRetracted) {
    updated = retractTimelineMessage(
      updated,
      existing.retractionId ?? incoming.retractionId,
    );
  }
  if (mergedIds.wireIds?.length) updated.wireIds = mergedIds.wireIds;
  else delete updated.wireIds;
  // #1182: the reconciled row is only still identity-less if both sides were.
  if (!(existing.synthesizedId && incoming.synthesizedId)) delete updated.synthesizedId;
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
  // #1182: content reconciliation bridges the fabricated-UUID gap in two
  // shapes only: a synthesized incoming (id-less redelivery onto a
  // MAM-seeded row) and an archive-decoded incoming (reconnect catch-up
  // emits archive rows through this path — #1190 skips the lifecycle
  // reload, so this is the only chance to reconcile the id-less
  // original). A genuinely new id-carrying *live* arrival is a distinct
  // message and must not be swallowed by a same-body synthesized row.
  const contentMatchable = msg.synthesizedId || msg.createdAtSource === "archive";
  const existing = findLiveMergeTarget(messages, msg, pendingEchoClientIds)
    ?? (contentMatchable ? findSynthesizedIdMergeTarget(messages, msg) : undefined);
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
