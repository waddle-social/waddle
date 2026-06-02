import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, type LiveRoomMessage } from "@/lib/xmpp-client";
import {
  inferredFileDisposition,
  type MarkupSpan,
  type MessageReference,
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
import { compareTimelineMessages, pickAuthoritativeTimestamp } from "@/lib/timeline-timestamps";

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

  const ids = mergeMessageIds(next, next.id, [incoming.id, ...(incoming.wireIds ?? [])]);
  if (ids.id !== next.id || !sameStringList(ids.wireIds, next.wireIds)) next = ids;

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

export function retractChannelTimelineMessage(
  existing: TimelineMessage,
  retractionId?: string,
): TimelineMessage {
  const next: TimelineMessage = {
    ...existing,
    body: "",
    isRetracted: true,
    ...(retractionId ? { retractionId } : {}),
  };
  delete next.markup;
  delete next.references;
  delete next.sharedFiles;
  delete next.linkPreviews;
  delete next.extensionAnnotations;
  delete next.extensionBodyFallback;
  delete next.isSticker;
  delete next.mentions;
  delete next.broadcastMention;
  delete next.forumPostKind;
  delete next.forumTitle;
  delete next.forumThreadTitle;
  return next;
}

function mergeRetractionTombstone(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): TimelineMessage {
  let result = incoming.isRetracted
    ? retractChannelTimelineMessage(existing, incoming.retractionId)
    : existing;
  // Upgrade the timestamp if the MAM hit carries a higher-authority
  // stamp than the row we already had — see `dms/message-timeline-state.ts`.
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
  return incoming.isRetracted ? result : mergeAuthoritativeLinkPreviews(result, incoming);
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

export function reactionSendersForUpdate(
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

export function removeSenderReactions(
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

export function reactionsFromSenders(
  reactionSenders: Record<string, Record<string, string>>,
): Record<string, string[]> {
  return Object.fromEntries(
    Object.entries(reactionSenders).map(([emoji, senders]) => [emoji, Object.values(senders)]),
  );
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
  const reactionUpdates: { targetId: string; nick: string; senderId: string; emojis: string[] }[] = [];
  const retractionUpdates: {
    targetId: string;
    retractionSender: { authorJid: string; authorRealJid?: string };
    isModeration: boolean;
  }[] = [];
  const correctionUpdates: {
    targetId: string;
    correctionSender: { authorJid: string; authorRealJid?: string };
    body: string;
    markup?: MarkupSpan[];
    references?: MessageReference[];
    linkPreviews?: LiveRoomMessage["linkPreviews"];
    extensionAnnotations?: LiveRoomMessage["extensionAnnotations"];
    extensionBodyFallback?: boolean;
  }[] = [];

  for (const msg of mamResults) {
    if (msg._reactionTarget && msg._reactionEmojis) {
      reactionUpdates.push({
        targetId: msg._reactionTarget,
        nick: msg.nick,
        senderId: msg._reactionSenderId ?? `${msg.roomJid}/${msg.nick}`,
        emojis: msg._reactionEmojis,
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
        body: msg.body,
        markup: msg.markup,
        references: msg.references,
        linkPreviews: msg.linkPreviews,
        extensionAnnotations: msg.extensionAnnotations,
        extensionBodyFallback: msg.extensionBodyFallback,
      });
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
  const timeline = options.seedExistingOnly ? [] : [...existing];
  for (const raw of regularMessages) {
    const mapped = mapLiveRoomMessageToTimeline(session, raw, (id) => byId.get(id));
    const tm = mapped.isRetracted
      ? retractChannelTimelineMessage(mapped, mapped.retractionId)
      : mapped;
    const existingMessage = [tm.id, ...(tm.wireIds ?? [])]
      .map((id) => byId.get(id))
      .find((message): message is TimelineMessage => !!message);
    if (existingMessage) {
      const mergedBase = options.seedExistingOnly
        ? mergeMissingThreadMetadata(tm, existingMessage)
        : mergeMissingThreadMetadata(existingMessage, tm);
      const merged = mergeRetractionTombstone(mergedBase, tm);
      if (options.seedExistingOnly) {
        byId.add(merged);
        timeline.push(merged);
      } else if (merged !== existingMessage) {
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
    if (!target || !isSameMucCorrectionSender(target, update.correctionSender)) continue;
    target.body = update.body;
    target.isEdited = true;
    if (update.markup && update.markup.length > 0) target.markup = update.markup;
    else delete target.markup;
    if (update.references && update.references.length > 0) target.references = update.references;
    else delete target.references;
    if (update.linkPreviews && update.linkPreviews.length > 0) target.linkPreviews = update.linkPreviews;
    else delete target.linkPreviews;
    if (update.extensionAnnotations && update.extensionAnnotations.length > 0) {
      target.extensionAnnotations = update.extensionAnnotations;
      if (update.extensionBodyFallback) target.extensionBodyFallback = true;
      else delete target.extensionBodyFallback;
    } else {
      delete target.extensionAnnotations;
      delete target.extensionBodyFallback;
    }
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
    const retracted = retractChannelTimelineMessage(target);
    timeline[index] = retracted;
    byId.add(retracted);
  }

  for (const update of reactionUpdates) {
    const target = timeline.find((message) => message.reactionTargetId === update.targetId);
    if (!target) continue;
    const reactionSenders = reactionSendersForUpdate(target, update.nick, update.senderId);
    removeSenderReactions(reactionSenders, update.nick, update.senderId);
    for (const emoji of update.emojis) {
      if (!reactionSenders[emoji]) reactionSenders[emoji] = {};
      reactionSenders[emoji][update.senderId] = update.nick;
    }
    const reactions = reactionsFromSenders(reactionSenders);
    if (Object.keys(reactionSenders).length > 0) {
      target.reactionSenders = reactionSenders;
      target.reactions = reactions;
    } else {
      delete target.reactionSenders;
      delete target.reactions;
    }
  }

  return applyForumContext(timeline.sort(compareTimelineMessages), channelIsForum);
}
