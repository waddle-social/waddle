import {
  inferredFileDisposition,
  type TimelineMessage,
} from "@/lib/chat-ui";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, type LiveDmMessage } from "@/lib/xmpp-client";
import type { PersistedQueuedDmMessage } from "@/lib/outbound-queue-store";
import {
  findMessageById,
  MessageIdIndex,
} from "@/lib/message-ids";
import { compareTimelineMessages } from "@/lib/timeline-timestamps";
import { assignCorrectionFields, type CorrectionPayload } from "@/lib/messaging/correction";
import { retractTimelineMessage } from "@/lib/messaging/retraction";
import { type ArchiveMergeHooks, mergeRetractionTombstone } from "@/lib/messaging/timeline-insert";

export function fromLiveDmMessage(
  session: WaddleSession,
  msg: LiveDmMessage,
  parentLookup?: (id: string) => { body?: string } | undefined,
): TimelineMessage {
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    authorJid: msg.fromJid,
    body: msg.body,
    createdAt: msg.createdAt,
    createdAtSource: msg.createdAtSource,
    isSelf: barePeerJid(msg.fromJid) === barePeerJid(session.jid),
  };
  if (msg.correctionTargetId) tm.correctionTargetId = msg.correctionTargetId;
  if (msg.replyableId) tm.replyableId = msg.replyableId;
  if (msg.stanzaId) tm.stanzaId = msg.stanzaId;
  if (msg.stanzaIdBy) tm.stanzaIdBy = msg.stanzaIdBy;
  if (msg.displayedMarkerRequested) tm.displayedMarkerRequested = true;
  if (msg.isRetracted) tm.isRetracted = true;
  if (msg.retractionId) tm.retractionId = msg.retractionId;
  if (msg.wireIds?.length) tm.wireIds = msg.wireIds;
  if (msg.mentions?.length) tm.mentions = msg.mentions;
  if (msg.markup?.length) tm.markup = msg.markup;
  if (msg.references?.length) tm.references = msg.references;
  if (msg.sharedFiles && msg.sharedFiles.length > 0) tm.sharedFiles = msg.sharedFiles;
  if (msg.linkPreviews && msg.linkPreviews.length > 0) tm.linkPreviews = msg.linkPreviews;
  if (msg.extensionAnnotations && msg.extensionAnnotations.length > 0) tm.extensionAnnotations = msg.extensionAnnotations;
  if (msg.extensionBodyFallback) tm.extensionBodyFallback = true;
  if (msg.isSticker) tm.isSticker = true;
  if (msg.replyTo) {
    const parent = parentLookup?.(msg.replyTo.id);
    tm.replyTo = {
      id: msg.replyTo.id,
      ...(msg.replyTo.author ? { author: msg.replyTo.author } : {}),
      ...(parent?.body ? { preview: parent.body } : {}),
    };
  }
  if (msg.threadId) tm.threadId = msg.threadId;
  if (msg.parentThreadId) tm.parentThreadId = msg.parentThreadId;
  if (msg.callThread) tm.callThread = msg.callThread;
  return tm.isRetracted ? retractTimelineMessage(tm, tm.retractionId) : tm;
}

export function queuedDmMessageToTimeline(
  session: WaddleSession,
  queued: PersistedQueuedDmMessage,
): TimelineMessage {
  const message: TimelineMessage = {
    id: queued.id,
    correctionTargetId: queued.id,
    author: session.username,
    authorJid: session.jid,
    body: queued.body || (queued.files?.[0]?.url ?? ""),
    createdAt: queued.createdAt,
    createdAtSource: "queued",
    isSelf: true,
    deliveryStatus: "queued",
  };
  if (queued.markup?.length) message.markup = queued.markup;
  if (queued.references?.length) message.references = queued.references;
  if (queued.replyTo) {
    message.replyTo = {
      id: queued.replyTo.id,
      ...(queued.replyTo.author ? { author: queued.replyTo.author } : {}),
      ...(queued.replyTo.body ? { preview: queued.replyTo.body } : {}),
    };
  }
  if (queued.threadId) message.threadId = queued.threadId;
  if (queued.parentThreadId) message.parentThreadId = queued.parentThreadId;
  if (queued.files && queued.files.length > 0) {
    message.sharedFiles = queued.files.map((file) => ({
      url: file.url,
      name: file.name,
      mediaType: file.mediaType,
      size: file.size,
      ...(file.width ? { width: file.width } : {}),
      ...(file.height ? { height: file.height } : {}),
      disposition: inferredFileDisposition(file.mediaType, file.name ?? file.url),
      ...(file.encrypted ? { encrypted: file.encrypted } : {}),
    }));
  }
  return message;
}

export function isSameDmCorrectionSender(target: TimelineMessage, correctionFromJid: string): boolean {
  return barePeerJid(target.authorJid ?? "") === barePeerJid(correctionFromJid);
}

// XEP-0444 nick-keyed aggregate bookkeeping — the DM side of reaction
// divergence 5. No real-JID sender attribution: the DM stanza already
// arrives keyed on the conversation partner, so the reaction-set per
// nick is sufficient. Replace semantics: the sender's previous set is
// wiped before the new one lands, so unticking an emoji removes it.
// Returns `undefined` when the resulting set is empty (drops the field).
function replacedNickReactions(
  current: Record<string, string[]> | undefined,
  nick: string,
  emojis: string[],
): Record<string, string[]> | undefined {
  const next: Record<string, string[]> = current ? { ...current } : {};
  for (const key of Object.keys(next)) {
    next[key] = (next[key] ?? []).filter((n) => n !== nick);
    if (next[key].length === 0) delete next[key];
  }
  for (const emoji of emojis) {
    if (!next[emoji]) next[emoji] = [];
    if (!next[emoji].includes(nick)) next[emoji].push(nick);
  }
  return Object.keys(next).length > 0 ? next : undefined;
}

/** Immutable variant of the DM reaction bookkeeping (live merge path). */
export function dmReactedRow(
  target: TimelineMessage,
  event: { nick: string; emojis: string[] },
): TimelineMessage {
  const reactions = replacedNickReactions(target.reactions, event.nick, event.emojis);
  const updated: TimelineMessage = { ...target };
  if (reactions) updated.reactions = reactions;
  else delete updated.reactions;
  return updated;
}

// DM-only extra on the shared MAM duplicate merge: adopt the archive
// row's call-thread payload (the channel side reconciles call anchors via
// the separate `callThreadEnded` pass instead).
const dmArchiveMergeHooks: ArchiveMergeHooks = {
  mergeExtras: (result, incoming) =>
    incoming.callThread ? { ...result, callThread: incoming.callThread } : result,
};

export function buildDmTimelineFromMamResults(params: {
  session: WaddleSession;
  mamResults: LiveDmMessage[];
  existing?: TimelineMessage[];
}): TimelineMessage[] {
  const { session, mamResults, existing = [] } = params;
  const regular: LiveDmMessage[] = [];
  const reactionUpdates: { targetId: string; nick: string; emojis: string[] }[] = [];
  const retractionUpdates: { targetId: string; fromJid: string }[] = [];
  const correctionUpdates: {
    targetId: string;
    correctionFromJid: string;
    payload: CorrectionPayload;
  }[] = [];
  for (const msg of mamResults) {
    if (msg._reactionTarget && msg._reactionEmojis) {
      reactionUpdates.push({ targetId: msg._reactionTarget, nick: msg.nick, emojis: msg._reactionEmojis });
    } else if (msg.retractsId) {
      retractionUpdates.push({ targetId: msg.retractsId, fromJid: msg.fromJid });
    } else if (msg.replacesId) {
      correctionUpdates.push({
        targetId: msg.replacesId,
        correctionFromJid: msg.fromJid,
        payload: {
          body: msg.body,
          markup: msg.markup,
          references: msg.references,
          linkPreviews: msg.linkPreviews,
          extensionAnnotations: msg.extensionAnnotations,
          extensionBodyFallback: msg.extensionBodyFallback,
        },
      });
    } else if (
      msg.body
      || msg.isRetracted
      || (msg.sharedFiles && msg.sharedFiles.length > 0)
      || msg.isSticker
      || (msg.linkPreviews && msg.linkPreviews.length > 0)
      || (msg.extensionAnnotations && msg.extensionAnnotations.length > 0)
      || msg.callThread
    ) {
      regular.push(msg);
    }
  }
  const byId = new MessageIdIndex<TimelineMessage>();
  for (const message of existing) byId.add(message);
  const timeline = [...existing];
  for (const raw of regular) {
    const tm = fromLiveDmMessage(session, raw, (id) => byId.get(id));
    // Look up via every wire alias, not just the primary id — when
    // the live arrival landed first with a client-side id and MAM
    // later returns the canonical stanza-id, a bare `findMessageById`
    // (on `tm.id` only) would miss the existing row and insert a
    // duplicate. Channel side has done this for a while; this is the
    // matching DM-side fix (Bug 5 in PR1).
    const existingMessage = [tm.id, ...(tm.wireIds ?? [])]
      .map((id) => byId.get(id))
      .find((message): message is TimelineMessage => !!message);
    if (existingMessage) {
      const merged = mergeRetractionTombstone(existingMessage, tm, dmArchiveMergeHooks);
      if (merged !== existingMessage) {
        const index = timeline.indexOf(existingMessage);
        if (index !== -1) timeline[index] = merged;
        byId.add(merged);
      }
      continue;
    }
    byId.add(tm);
    timeline.push(tm);
  }
  for (const update of correctionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target || !isSameDmCorrectionSender(target, update.correctionFromJid)) continue;
    assignCorrectionFields(target, update.payload);
  }
  for (const update of retractionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target || !isSameDmCorrectionSender(target, update.fromJid)) continue;
    const index = timeline.indexOf(target);
    if (index === -1) continue;
    const retracted = retractTimelineMessage(target);
    timeline[index] = retracted;
    byId.add(retracted);
  }
  for (const update of reactionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target) continue;
    const reactions = replacedNickReactions(target.reactions, update.nick, update.emojis);
    if (reactions) target.reactions = reactions;
    else delete target.reactions;
  }
  return timeline.sort(compareTimelineMessages);
}
