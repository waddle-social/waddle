import {
  inferredFileDisposition,
  type TimelineMessage,
} from "@/lib/chat-ui";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid, type LiveDmMessage } from "@/lib/xmpp-client";
import type { PersistedQueuedDmMessage } from "@/lib/outbound-queue-store";
import {
  findMessageById,
  indexMessageByIds,
} from "@/lib/message-ids";

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
    isSelf: barePeerJid(msg.fromJid) === barePeerJid(session.jid),
  };
  if (msg.correctionTargetId) tm.correctionTargetId = msg.correctionTargetId;
  if (msg.replyableId) tm.replyableId = msg.replyableId;
  if (msg.isRetracted) tm.isRetracted = true;
  if (msg.retractionId) tm.retractionId = msg.retractionId;
  if (msg.wireIds?.length) tm.wireIds = msg.wireIds;
  if (msg.mentions?.length) tm.mentions = msg.mentions;
  if (msg.markup?.length) tm.markup = msg.markup;
  if (msg.references?.length) tm.references = msg.references;
  if (msg.sharedFiles && msg.sharedFiles.length > 0) tm.sharedFiles = msg.sharedFiles;
  if (msg.extensionAnnotations && msg.extensionAnnotations.length > 0) tm.extensionAnnotations = msg.extensionAnnotations;
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
  return tm.isRetracted ? retractDmTimelineMessage(tm, tm.retractionId) : tm;
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

export function retractDmTimelineMessage(
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
  delete next.extensionAnnotations;
  delete next.isSticker;
  delete next.mentions;
  delete next.broadcastMention;
  delete next.forumPostKind;
  delete next.forumTitle;
  delete next.forumThreadTitle;
  return next;
}

function mergeDmRetractionTombstone(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): TimelineMessage {
  return incoming.isRetracted ? retractDmTimelineMessage(existing, incoming.retractionId) : existing;
}

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
    body: string;
    markup?: LiveDmMessage["markup"];
    references?: LiveDmMessage["references"];
    extensionAnnotations?: LiveDmMessage["extensionAnnotations"];
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
        body: msg.body,
        markup: msg.markup,
        references: msg.references,
        extensionAnnotations: msg.extensionAnnotations,
      });
    } else if (
      msg.body
      || msg.isRetracted
      || (msg.sharedFiles && msg.sharedFiles.length > 0)
      || msg.isSticker
      || (msg.extensionAnnotations && msg.extensionAnnotations.length > 0)
    ) {
      regular.push(msg);
    }
  }
  const byId = new Map<string, TimelineMessage>();
  for (const message of existing) indexMessageByIds(byId, message);
  const timeline = [...existing];
  for (const raw of regular) {
    const tm = fromLiveDmMessage(session, raw, (id) => byId.get(id));
    const existingMessage = findMessageById(timeline, tm.id);
    if (existingMessage) {
      const merged = mergeDmRetractionTombstone(existingMessage, tm);
      if (merged !== existingMessage) {
        const index = timeline.indexOf(existingMessage);
        if (index !== -1) timeline[index] = merged;
        indexMessageByIds(byId, merged);
      }
      continue;
    }
    indexMessageByIds(byId, tm);
    timeline.push(tm);
  }
  for (const update of correctionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target || !isSameDmCorrectionSender(target, update.correctionFromJid)) continue;
    target.body = update.body;
    target.isEdited = true;
    if (update.markup && update.markup.length > 0) target.markup = update.markup;
    else delete target.markup;
    if (update.references && update.references.length > 0) target.references = update.references;
    else delete target.references;
    if (update.extensionAnnotations && update.extensionAnnotations.length > 0) {
      target.extensionAnnotations = update.extensionAnnotations;
    } else {
      delete target.extensionAnnotations;
    }
  }
  for (const update of retractionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target || !isSameDmCorrectionSender(target, update.fromJid)) continue;
    const index = timeline.indexOf(target);
    if (index === -1) continue;
    const retracted = retractDmTimelineMessage(target);
    timeline[index] = retracted;
    indexMessageByIds(byId, retracted);
  }
  for (const update of reactionUpdates) {
    const target = findMessageById(timeline, update.targetId);
    if (!target) continue;
    const reactions: Record<string, string[]> = target.reactions ? { ...target.reactions } : {};
    for (const key of Object.keys(reactions)) {
      reactions[key] = (reactions[key] ?? []).filter((n) => n !== update.nick);
      if (reactions[key].length === 0) delete reactions[key];
    }
    for (const emoji of update.emojis) {
      if (!reactions[emoji]) reactions[emoji] = [];
      if (!reactions[emoji].includes(update.nick)) reactions[emoji].push(update.nick);
    }
    if (Object.keys(reactions).length > 0) target.reactions = reactions;
    else delete target.reactions;
  }
  return timeline.sort((a, b) => a.createdAt.localeCompare(b.createdAt));
}
