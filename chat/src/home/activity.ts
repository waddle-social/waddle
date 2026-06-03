import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp-client";

interface ChannelUnreadState {
  unread: number;
  mentions: number;
  threadUnread?: number;
  preview?: string;
  lastUpdated?: number;
}

export interface ChannelActivityState extends ChannelUnreadState {
  hasActivity: boolean;
}

export interface HomeActivitySummary extends ChannelActivityState {
  channelCount: number;
}

export type ChannelUnreadMap = Record<string, ChannelUnreadState>;

function bareJid(jid: string): string {
  return jid.split("/")[0] ?? jid;
}

function hasRoomActivity(channel: Pick<ChannelSummary, "id" | "jid">, activeChannelJids: ReadonlySet<string>): boolean {
  if (!channel.jid) return false;
  const roomJid = bareJid(channel.jid);
  for (const jid of activeChannelJids) {
    const activeJid = bareJid(jid);
    if (activeJid === roomJid) return true;
  }
  return false;
}

export function channelActivityState(
  channel: Pick<ChannelSummary, "id" | "jid">,
  unreadMap: ChannelUnreadMap | undefined,
  activeChannelJids: ReadonlySet<string>,
): ChannelActivityState {
  const unread = unreadMap?.[channel.id] ?? { unread: 0, mentions: 0 };
  return {
    unread: unread.unread,
    mentions: unread.mentions,
    threadUnread: unread.threadUnread ?? 0,
    ...(unread.preview ? { preview: unread.preview } : {}),
    ...(unread.lastUpdated ? { lastUpdated: unread.lastUpdated } : {}),
    hasActivity: hasRoomActivity(channel, activeChannelJids),
  };
}

export function summarizeHomeActivity(
  channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  unreadMap: ChannelUnreadMap | undefined,
  activeChannelJids: ReadonlySet<string>,
): HomeActivitySummary {
  return channels.reduce<HomeActivitySummary>((summary, channel) => {
    const activity = channelActivityState(channel, unreadMap, activeChannelJids);
    const latest = isNewerActivity(activity, summary) ? activity : summary;
    return {
      channelCount: summary.channelCount + 1,
      unread: summary.unread + activity.unread,
      mentions: summary.mentions + activity.mentions,
      threadUnread: (summary.threadUnread ?? 0) + (activity.threadUnread ?? 0),
      ...(latest.preview ? { preview: latest.preview } : {}),
      ...(latest.lastUpdated ? { lastUpdated: latest.lastUpdated } : {}),
      hasActivity: summary.hasActivity || activity.hasActivity,
    };
  }, { channelCount: 0, unread: 0, mentions: 0, threadUnread: 0, hasActivity: false });
}

export function homeActivityLabel(activity: ChannelActivityState): string {
  const parts: string[] = [];
  if (activity.mentions > 0) {
    parts.push(`${activity.mentions} ${activity.mentions === 1 ? "mention" : "mentions"}`);
  }
  if (activity.unread > 0) {
    parts.push(`${activity.unread} unread`);
  }
  if ((activity.threadUnread ?? 0) > 0) {
    const threadUnread = activity.threadUnread ?? 0;
    parts.push(`${threadUnread} ${threadUnread === 1 ? "thread reply" : "thread replies"}`);
  }
  if (activity.unread === 0 && (activity.threadUnread ?? 0) === 0 && activity.preview) {
    parts.push("recent activity");
  }
  if (activity.hasActivity) {
    parts.push("live activity");
  }
  return parts.length > 0 ? parts.join(", ") : "no unread activity";
}

export function channelHomeLabel(channel: ChannelSummary, activity: ChannelActivityState, recency?: string): string {
  const preview = channelActivityPreview(activity);
  return [
    `${channel.name}, channel`,
    homeActivityLabel(activity),
    ...(preview ? [`preview: ${preview}`] : []),
    ...(recency ? [`last updated: ${recency}`] : []),
  ].join(", ");
}

export function spaceHomeLabel(spaceName: string, summary: HomeActivitySummary, targetChannelName?: string): string {
  const channelCount = `${summary.channelCount} ${summary.channelCount === 1 ? "channel" : "channels"}`;
  const preview = channelActivityPreview(summary);
  return [
    spaceName,
    ...(targetChannelName ? [`opens ${targetChannelName}`] : []),
    channelCount,
    homeActivityLabel(summary),
    ...(preview ? [`preview: ${preview}`] : []),
  ].join(", ");
}

export function channelActivityPreview(activity: ChannelActivityState): string {
  if (activity.preview) return dmPreviewText(activity.preview, 72);
  if ((activity.threadUnread ?? 0) > 0) {
    const threadUnread = activity.threadUnread ?? 0;
    return `${threadUnread} unread ${threadUnread === 1 ? "thread reply" : "thread replies"}`;
  }
  return "";
}

export function channelUnreadBadgeCount(activity: Pick<ChannelActivityState, "unread" | "threadUnread">): number {
  return activity.unread;
}

export function compareChannelActivityPriority(a: ChannelActivityState, b: ChannelActivityState): number {
  if (a.mentions !== b.mentions) return b.mentions - a.mentions;
  const unreadDiff = channelActivityCount(b) - channelActivityCount(a);
  if (unreadDiff !== 0) return unreadDiff;
  if (a.hasActivity !== b.hasActivity) return a.hasActivity ? -1 : 1;
  return (b.lastUpdated ?? 0) - (a.lastUpdated ?? 0);
}

function channelActivityCount(activity: Pick<ChannelActivityState, "unread" | "threadUnread">): number {
  return activity.unread + (activity.threadUnread ?? 0);
}

export function dmPreviewText(body: string | undefined, maxLength = 64): string {
  if (!body) return "";
  const trimmed = body.trim();
  return trimmed.length > maxLength ? `${trimmed.slice(0, maxLength)}...` : trimmed;
}

export function dmPresenceLabel(show?: DmConversation["presenceShow"]): string {
  if (show === "available") return "available";
  if (show === "away") return "away";
  if (show === "xa") return "extended away";
  if (show === "dnd") return "do not disturb";
  return "offline";
}

export function dmHomeLabel(
  conversation: Pick<DmConversation, "peerUsername" | "peerJid" | "lastMessageBody" | "unreadCount" | "presenceShow">,
  recency?: string,
): string {
  const label = conversation.peerUsername && conversation.peerUsername !== conversation.peerJid
    ? `${conversation.peerUsername} (${conversation.peerJid})`
    : conversation.peerJid;
  const activity = conversation.unreadCount > 0
    ? `${conversation.unreadCount} unread`
    : conversation.lastMessageBody
      ? "recent activity"
      : "no unread activity";
  const preview = dmPreviewText(conversation.lastMessageBody);
  const presence = dmPresenceLabel(conversation.presenceShow);
  return [
    `${label}, direct message`,
    presence,
    activity,
    ...(preview ? [`last message: ${preview}`] : []),
    ...(recency ? [`last updated: ${recency}`] : []),
  ].join(", ");
}

function isNewerActivity(candidate: ChannelActivityState, current: ChannelActivityState): boolean {
  return (candidate.lastUpdated ?? 0) > (current.lastUpdated ?? 0);
}
