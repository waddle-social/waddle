import type { TimelineMessage } from "@/lib/chat-ui";
import type { WaddleSession } from "@/lib/server-auth";
import type { LiveRoomMessage } from "@/lib/xmpp-client";

export function mapLiveRoomMessageToTimeline(
  session: WaddleSession,
  msg: LiveRoomMessage,
  parentLookup?: (id: string) => { body?: string } | undefined,
): TimelineMessage {
  const authorOccupantJid = `${msg.roomJid}/${msg.nick}`;
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    authorJid: msg.authorRealJid ?? authorOccupantJid,
    authorOccupantJid,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: msg.nick === session.username,
  };
  if (msg.correctionTargetId) tm.correctionTargetId = msg.correctionTargetId;
  if (msg.reactionTargetId) tm.reactionTargetId = msg.reactionTargetId;
  if (msg.replyableId) tm.replyableId = msg.replyableId;
  if (msg.stanzaId) tm.stanzaId = msg.stanzaId;
  if (msg.stanzaIdBy) tm.stanzaIdBy = msg.stanzaIdBy;
  if (msg.isRetracted) tm.isRetracted = true;
  if (msg.retractionId) tm.retractionId = msg.retractionId;
  if (msg.authorRealJid) tm.authorRealJid = msg.authorRealJid;
  if (msg.wireIds && msg.wireIds.length > 0) tm.wireIds = msg.wireIds;
  if (msg.mentions && msg.mentions.length > 0) tm.mentions = msg.mentions;
  if (msg.sharedFiles && msg.sharedFiles.length > 0) tm.sharedFiles = msg.sharedFiles;
  if (msg.extensionAnnotations && msg.extensionAnnotations.length > 0) {
    tm.extensionAnnotations = msg.extensionAnnotations;
  }
  if (msg.extensionBodyFallback) tm.extensionBodyFallback = true;
  if (msg.isSticker) tm.isSticker = true;
  if (msg.broadcastMention) tm.broadcastMention = msg.broadcastMention;
  if (msg.markup && msg.markup.length > 0) tm.markup = msg.markup;
  if (msg.references && msg.references.length > 0) tm.references = msg.references;
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
  if (msg.forumPostKind) tm.forumPostKind = msg.forumPostKind;
  if (msg.forumTitle) tm.forumTitle = msg.forumTitle;
  if (msg.forumThreadTitle) tm.forumThreadTitle = msg.forumThreadTitle;
  if (msg.type === "pin-event") {
    tm.isPinEvent = true;
    if (msg.pinEventAction) tm.pinEventAction = msg.pinEventAction;
  }
  return tm;
}

export function applyForumContext(list: TimelineMessage[], inferForumReplies = true): TimelineMessage[] {
  const threadTitles = new Map<string, string>();

  for (const message of list) {
    if (!message.isRetracted && message.forumPostKind === "topic" && message.forumTitle) {
      threadTitles.set(message.threadId ?? message.id, message.forumTitle);
    }
  }

  return list.map((message) => {
    if (message.isRetracted) return message;
    const threadId = message.threadId ?? (message.forumPostKind === "topic" ? message.id : undefined);
    const threadTitle = threadId ? threadTitles.get(threadId) : undefined;
    const next: TimelineMessage = { ...message };
    let changed = false;

    if (message.forumPostKind === "topic") {
      if (threadId && message.threadId !== threadId) {
        next.threadId = threadId;
        changed = true;
      }
      if (message.forumTitle && message.forumThreadTitle !== message.forumTitle) {
        next.forumThreadTitle = message.forumTitle;
        changed = true;
      }
    } else if (threadId) {
      if (inferForumReplies && !message.forumPostKind && message.id !== threadId) {
        next.forumPostKind = "reply";
        changed = true;
      }
      const forumThreadTitle = threadTitle
        ?? (next.forumPostKind === "reply" && !message.body ? `Thread ${threadId}` : undefined);
      if (forumThreadTitle && message.forumThreadTitle !== forumThreadTitle) {
        next.forumThreadTitle = forumThreadTitle;
        changed = true;
      }
    }

    return changed ? next : message;
  });
}

export function isFeedTimelineMessage(message: TimelineMessage): boolean {
  return !message.threadId || message.id === message.threadId;
}

const TIMELINE_STAMP_FORMATTER = new Intl.DateTimeFormat("en", {
  hour: "2-digit",
  minute: "2-digit",
  month: "short",
  day: "numeric",
  hour12: false,
});

const TIMELINE_TIME_OF_DAY_FORMATTER = new Intl.DateTimeFormat("en", {
  hour: "2-digit",
  minute: "2-digit",
  hour12: false,
});

export function formatTimelineStamp(value: string) {
  return TIMELINE_STAMP_FORMATTER.format(new Date(value));
}

export function formatTimelineTimeOfDay(value: string) {
  return TIMELINE_TIME_OF_DAY_FORMATTER.format(new Date(value));
}

const DAY_DIVIDER_FORMATTER = new Intl.DateTimeFormat("en", {
  weekday: "short",
  month: "short",
  day: "numeric",
});

function localDayOrdinal(date: Date): number {
  return Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / (24 * 60 * 60 * 1000);
}

export function formatTimelineDayDivider(value: string, now = new Date()) {
  const date = new Date(value);
  const dayOffset = localDayOrdinal(now) - localDayOrdinal(date);
  if (dayOffset === 0) return "Today";
  if (dayOffset === 1) return "Yesterday";
  return DAY_DIVIDER_FORMATTER.format(date);
}

export function isSameTimelineDay(a: string, b: string): boolean {
  return localDayOrdinal(new Date(a)) === localDayOrdinal(new Date(b));
}
