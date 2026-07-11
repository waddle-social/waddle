import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, type LiveRoomMessage } from "@/lib/xmpp-client";
import {
  inferredFileDisposition,
  type TimelineMessage,
} from "@/lib/chat-ui";
import type { PersistedQueuedRoomMessage } from "@/lib/outbound-queue-store";
import {
  findMessageById,
  MessageIdIndex,
  mergeMessageIds,
} from "@/lib/message-ids";
import {
  applyForumContext,
  mapLiveRoomMessageToTimeline,
} from "@/channels/timeline";
import { compareTimelineMessages } from "@/lib/timeline-timestamps";
import { assignCorrectionFields, type CorrectionPayload } from "@/lib/messaging/correction";
import { isStaleReactionUpdate } from "@/lib/messaging/reactions";
import { retractTimelineMessage } from "@/lib/messaging/retraction";
import { mergeRetractionTombstone } from "@/lib/messaging/timeline-insert";
import { adoptArchiveIdentity, findSynthesizedIdMergeTarget } from "@/lib/messaging/synthesized-id-merge";
import { SenderScopedIdIndex } from "@/lib/messaging/sender-scoped-ids";

function mergeReplyToMetadata(
  existing: TimelineMessage["replyTo"],
  incoming: TimelineMessage["replyTo"],
): TimelineMessage["replyTo"] {
  if (!incoming) return existing;
  if (!existing) return { ...incoming };
  if (existing.id !== incoming.id) return existing;

  let next = existing;
  if (!next.author && incoming.author) next = { ...next, author: incoming.author };
  if (!next.preview && incoming.preview) next = { ...next, preview: incoming.preview };
  return next;
}

function sameStringList(a: readonly string[] | undefined, b: readonly string[] | undefined): boolean {
  const left = a ?? [];
  const right = b ?? [];
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function mergeMissingThreadMetadata(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): TimelineMessage {
  let next = existing;
  const assign = (patch: Partial<TimelineMessage>) => {
    next = next === existing ? { ...existing, ...patch } : { ...next, ...patch };
  };

  const canonicalRoomMessage = [incoming, existing].find((message) =>
    !!message.authorOccupantJid
    && !!message.stanzaId
    && !!message.stanzaIdBy
  );
  const ids = mergeMessageIds(
    next,
    canonicalRoomMessage?.stanzaId ?? next.id,
    [incoming.id, ...(incoming.wireIds ?? [])],
  );
  if (ids.id !== next.id || !sameStringList(ids.wireIds, next.wireIds)) next = ids;

  if (canonicalRoomMessage) {
    assign({
      stanzaId: canonicalRoomMessage.stanzaId,
      stanzaIdBy: canonicalRoomMessage.stanzaIdBy,
      ...(canonicalRoomMessage.replyableId
        ? { replyableId: canonicalRoomMessage.replyableId }
        : {}),
      ...(canonicalRoomMessage.reactionTargetId
        ? { reactionTargetId: canonicalRoomMessage.reactionTargetId }
        : {}),
      ...(canonicalRoomMessage.correctionTargetId
        ? { correctionTargetId: canonicalRoomMessage.correctionTargetId }
        : {}),
      ...(canonicalRoomMessage.authorRealJid
        ? {
            authorJid: canonicalRoomMessage.authorRealJid,
            authorRealJid: canonicalRoomMessage.authorRealJid,
          }
        : {}),
      authorOccupantJid: canonicalRoomMessage.authorOccupantJid,
      ...(canonicalRoomMessage.displayedMarkerRequested
        ? { displayedMarkerRequested: true }
        : {}),
    });
  }

  if (!next.threadId && incoming.threadId) assign({ threadId: incoming.threadId });
  if (!next.parentThreadId && incoming.parentThreadId) assign({ parentThreadId: incoming.parentThreadId });
  if (!next.correctionTargetId && incoming.correctionTargetId) {
    assign({ correctionTargetId: incoming.correctionTargetId });
  }
  if (!next.reactionTargetId && incoming.reactionTargetId) {
    assign({ reactionTargetId: incoming.reactionTargetId });
  }

  const replyTo = mergeReplyToMetadata(next.replyTo, incoming.replyTo);
  if (replyTo !== next.replyTo) assign({ replyTo });

  return next;
}

export interface TimelineBuildOptions {
  seedExistingOnly?: boolean;
}

export function queuedRoomMessageToTimeline(
  session: WaddleSession,
  roomJid: string,
  queued: PersistedQueuedRoomMessage,
): TimelineMessage {
  const message: TimelineMessage = {
    id: queued.id,
    correctionTargetId: queued.id,
    author: session.username,
    authorJid: `${roomJid}/${session.username}`,
    body: queued.body || (queued.files?.[0]?.url ?? ""),
    createdAt: queued.createdAt,
    createdAtSource: "queued",
    isSelf: true,
    deliveryStatus: "queued",
  };
  if (queued.markup && queued.markup.length > 0) message.markup = queued.markup;
  if (queued.references && queued.references.length > 0) message.references = queued.references;
  if (queued.replyTo) {
    message.replyTo = {
      id: queued.replyTo.id,
      ...(queued.replyTo.author ? { author: queued.replyTo.author } : {}),
      ...(queued.replyTo.body ? { preview: queued.replyTo.body } : {}),
    };
  }
  if (queued.threadId) message.threadId = queued.threadId;
  if (queued.parentThreadId) message.parentThreadId = queued.parentThreadId;
  if (queued.threadCreate) {
    message.threadId = queued.id;
    message.forumPostKind = "topic";
    message.forumTitle = queued.threadCreate.title;
    message.forumThreadTitle = queued.threadCreate.title;
  } else if (queued.threadReply) {
    message.forumPostKind = "reply";
  }
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

function reactionSendersForUpdate(
  message: TimelineMessage,
  nick?: string,
  senderId?: string,
): Record<string, Record<string, string>> {
  const reactionSenders: Record<string, Record<string, string>> = {};
  for (const [emoji, senders] of Object.entries(message.reactionSenders ?? {})) {
    reactionSenders[emoji] = { ...senders };
  }
  if (Object.keys(reactionSenders).length > 0) return reactionSenders;
  for (const [emoji, nicks] of Object.entries(message.reactions ?? {})) {
    reactionSenders[emoji] = {};
    for (const existingNick of nicks) {
      const legacySenderId = existingNick === nick && senderId ? senderId : existingNick;
      reactionSenders[emoji][legacySenderId] = existingNick;
    }
  }
  return reactionSenders;
}

function removeSenderReactions(
  reactionSenders: Record<string, Record<string, string>>,
  nick: string,
  senderId: string,
) {
  for (const key of Object.keys(reactionSenders)) {
    for (const existingSenderId of Object.keys(reactionSenders[key])) {
      if (
        existingSenderId === senderId ||
        existingSenderId === nick ||
        existingSenderId.endsWith(`/${nick}`)
      ) {
        delete reactionSenders[key][existingSenderId];
      }
    }
    if (Object.keys(reactionSenders[key]).length === 0) delete reactionSenders[key];
  }
}

function reactionsFromSenders(
  reactionSenders: Record<string, Record<string, string>>,
): Record<string, string[]> {
  return Object.fromEntries(
    Object.entries(reactionSenders).map(([emoji, senders]) => [emoji, Object.values(senders)]),
  );
}

// XEP-0444 occupant-keyed sender bookkeeping — the channel side of
// reaction divergence 5. Replace semantics: this author's full reaction
// set on the target becomes `emojis`; the previous per-author set is
// wiped via `removeSenderReactions` so unticking an emoji actually
// removes it.
function channelReactionState(
  target: TimelineMessage,
  event: { nick: string; senderId: string; emojis: string[] },
): { reactionSenders: Record<string, Record<string, string>>; reactions: Record<string, string[]> } | null {
  const reactionSenders = reactionSendersForUpdate(target, event.nick, event.senderId);
  removeSenderReactions(reactionSenders, event.nick, event.senderId);
  for (const emoji of event.emojis) {
    if (!reactionSenders[emoji]) reactionSenders[emoji] = {};
    reactionSenders[emoji][event.senderId] = event.nick;
  }
  if (Object.keys(reactionSenders).length === 0) return null;
  return { reactionSenders, reactions: reactionsFromSenders(reactionSenders) };
}

/** Immutable variant of the channel reaction bookkeeping (live merge path). */
export function channelReactedRow(
  target: TimelineMessage,
  event: { nick: string; senderId: string; emojis: string[] },
): TimelineMessage {
  const state = channelReactionState(target, event);
  const updated: TimelineMessage = { ...target };
  if (state) {
    updated.reactionSenders = state.reactionSenders;
    updated.reactions = state.reactions;
  } else {
    delete updated.reactionSenders;
    delete updated.reactions;
  }
  return updated;
}

export function mucCorrectionSender(msg: Pick<LiveRoomMessage, "roomJid" | "nick" | "authorRealJid">): {
  authorJid: string;
  authorRealJid?: string;
} {
  return {
    authorJid: `${msg.roomJid}/${msg.nick}`,
    ...(msg.authorRealJid ? { authorRealJid: msg.authorRealJid } : {}),
  };
}

export function isSameMucCorrectionSender(
  target: TimelineMessage,
  correction: { authorJid: string; authorRealJid?: string },
): boolean {
  if ((target.authorOccupantJid ?? target.authorJid) !== correction.authorJid) return false;
  if (target.authorRealJid && correction.authorRealJid) {
    return barePeerJid(target.authorRealJid) === barePeerJid(correction.authorRealJid);
  }
  return true;
}

export function isSameMucRetractionSender(
  target: TimelineMessage,
  retraction: { authorJid: string; authorRealJid?: string },
): boolean {
  return isSameMucCorrectionSender(target, retraction);
}

export function isValidMucModerationTarget(target: TimelineMessage, targetId: string): boolean {
  return target.replyableId === targetId;
}

export function isValidMucRetractionTarget(target: TimelineMessage, targetId: string): boolean {
  return !target.replyableId || target.replyableId === targetId;
}

export function isMucServiceModeration(
  msg: Pick<LiveRoomMessage, "fromJid" | "roomJid" | "moderationTargetId">,
): boolean {
  return !!msg.moderationTargetId && msg.fromJid === msg.roomJid;
}

export function buildChannelTimelineFromMamResults(params: {
  session: WaddleSession;
  channelIsForum: boolean;
  mamResults: LiveRoomMessage[];
  existing?: TimelineMessage[];
  options?: TimelineBuildOptions;
}): TimelineMessage[] {
  const { session, channelIsForum, mamResults, existing = [], options = {} } = params;
  const regularMessages: LiveRoomMessage[] = [];
  const reactionUpdates: { targetId: string; nick: string; senderId: string; emojis: string[]; occurredAt?: string }[] = [];
  const retractionUpdates: {
    targetId: string;
    retractionSender: { authorJid: string; authorRealJid?: string };
    isModeration: boolean;
  }[] = [];
  const correctionUpdates: {
    targetId: string;
    correctionSender: { authorJid: string; authorRealJid?: string };
    payload: CorrectionPayload;
  }[] = [];
  const callThreadEndedUpdates: NonNullable<LiveRoomMessage["callThreadEnded"]>[] = [];

  for (const msg of mamResults) {
    if (msg._reactionTarget && msg._reactionEmojis) {
      reactionUpdates.push({
        targetId: msg._reactionTarget,
        nick: msg.nick,
        senderId: msg._reactionSenderId ?? `${msg.roomJid}/${msg.nick}`,
        emojis: msg._reactionEmojis,
        ...(msg.createdAt ? { occurredAt: msg.createdAt } : {}),
      });
    } else if (msg.retractsId) {
      if (msg.moderationTargetId && !isMucServiceModeration(msg)) continue;
      retractionUpdates.push({
        targetId: msg.retractsId,
        retractionSender: mucCorrectionSender(msg),
        isModeration: !!msg.moderationTargetId,
      });
    } else if (msg.replacesId) {
      correctionUpdates.push({
        targetId: msg.replacesId,
        correctionSender: mucCorrectionSender(msg),
        payload: {
          body: msg.body,
          markup: msg.markup,
          references: msg.references,
          linkPreviews: msg.linkPreviews,
          extensionAnnotations: msg.extensionAnnotations,
          extensionBodyFallback: msg.extensionBodyFallback,
        },
      });
    } else if (msg.callThreadEnded) {
      callThreadEndedUpdates.push(msg.callThreadEnded);
    } else if (
      msg.body
      || msg.isRetracted
      || (msg.sharedFiles && msg.sharedFiles.length > 0)
      || msg.isSticker
      || (msg.linkPreviews && msg.linkPreviews.length > 0)
      || msg.replyTo
      || msg.forumPostKind
      || (msg.extensionAnnotations && msg.extensionAnnotations.length > 0)
    ) {
      // NB: `msg.threadId` is intentionally absent from this gate.
      // XEP-0201 `<thread/>` is scope metadata, not content — see
      // `wasm-message-codecs.ts` (`roomMessageFromArchived`) and
      // `server/.../room/archive.rs` (`is_archivable`). A stanza whose
      // only "content" is a thread reference is a chat-state /
      // displayed-marker / etc. echoed per XEP-0201 §3 and must not
      // materialise as a timeline row.
      regularMessages.push(msg);
    }
  }

  const byId = new MessageIdIndex<TimelineMessage>();
  for (const message of existing) {
    byId.add(message);
  }
  const identityIndex = new SenderScopedIdIndex(existing);
  const timeline = options.seedExistingOnly ? [] : [...existing];
  // #1182: rows whose primary id had to be fabricated (id-less live
  // stanzas) can only reconcile by content. Each is consumable once so
  // two identical archive hits can't both collapse into it.
  const synthesizedRows = existing.filter((message) => message.synthesizedId);
  for (const raw of regularMessages) {
    const mapped = mapLiveRoomMessageToTimeline(session, raw, (id) => byId.get(id));
    const tm = mapped.isRetracted
      ? retractTimelineMessage(mapped, mapped.retractionId)
      : mapped;
    const idMatch = identityIndex.find(tm);
    const synthesizedMatch = idMatch ? undefined : findSynthesizedIdMergeTarget(synthesizedRows, tm);
    if (synthesizedMatch) synthesizedRows.splice(synthesizedRows.indexOf(synthesizedMatch), 1);
    const existingMessage = idMatch ?? synthesizedMatch;
    if (existingMessage) {
      const mergedBase = options.seedExistingOnly
        ? mergeMissingThreadMetadata(tm, existingMessage)
        : mergeMissingThreadMetadata(existingMessage, tm);
      let merged = mergeRetractionTombstone(mergedBase, tm);
      if (synthesizedMatch) merged = adoptArchiveIdentity(merged, tm);
      if (options.seedExistingOnly) {
        byId.add(merged);
        timeline.push(merged);
      } else if (merged !== existingMessage) {
        const index = timeline.indexOf(existingMessage);
        if (index !== -1) timeline[index] = merged;
        byId.add(merged);
      }
      identityIndex.replace(existingMessage, merged);
      continue;
    }
    byId.add(tm);
    identityIndex.add(tm);
    timeline.push(tm);
  }

  for (const update of callThreadEndedUpdates) {
    const target = timeline.find((message) =>
      !!message.callThread
      && (
        message.replyableId === update.anchorId
        || message.stanzaId === update.anchorId
        || message.id === update.anchorId
        || message.wireIds?.includes(update.anchorId)
      )
    );
    if (!target?.callThread) continue;
    target.callThread = {
      ...target.callThread,
      ended: update.ended,
      duration: update.duration,
    };
  }

  for (const update of correctionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target || !isSameMucCorrectionSender(target, update.correctionSender)) continue;
    assignCorrectionFields(target, update.payload);
  }

  for (const update of retractionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target) continue;
    if (update.isModeration) {
      if (!isValidMucModerationTarget(target, update.targetId)) continue;
    } else if (
      !isValidMucRetractionTarget(target, update.targetId)
      || !isSameMucRetractionSender(target, update.retractionSender)
    ) {
      continue;
    }
    const index = timeline.indexOf(target);
    if (index === -1) continue;
    const retracted = retractTimelineMessage(target);
    timeline[index] = retracted;
    byId.add(retracted);
  }

  for (const update of reactionUpdates) {
    const target = timeline.find((message) => message.reactionTargetId === update.targetId);
    if (!target) continue;
    // C5: XEP-0444 recency — an archived reaction older than one
    // already applied for the same sender must not clobber it. Keyed by
    // senderId (the replacement key), NOT nick — nicks are not stable.
    if (isStaleReactionUpdate(target, {
      senderKey: update.senderId,
      nick: update.nick,
      emojis: update.emojis,
      ...(update.occurredAt ? { occurredAt: update.occurredAt } : {}),
    })) continue;
    const state = channelReactionState(target, update);
    if (state) {
      target.reactionSenders = state.reactionSenders;
      target.reactions = state.reactions;
    } else {
      delete target.reactionSenders;
      delete target.reactions;
    }
    if (update.occurredAt) {
      target.reactionTimes = { ...(target.reactionTimes ?? {}), [update.senderId]: update.occurredAt };
    }
  }

  return applyForumContext(timeline.sort(compareTimelineMessages), channelIsForum);
}
