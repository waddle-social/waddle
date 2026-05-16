import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import type { DmConversation, FeedEntry, Story } from "@/lib/xmpp-client";
import type { ChannelUnreadMap } from "@/home/activity";

export interface HomeDashboardFeedState {
  entries: readonly FeedEntry[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

export interface HomeDashboardStoriesState {
  stories: readonly Story[];
  isLoading: boolean;
  isPosting: boolean;
  error: string | null;
  canPost: boolean;
  selfJid: string | null;
}

export interface HomeDashboardProps {
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  contacts: RosterContact[];
  isLoading: boolean;
  channelUnreadMap?: ChannelUnreadMap;
  activeChannelJids?: Set<string>;
  dmConversations?: DmConversation[];
  feed?: HomeDashboardFeedState;
  stories?: HomeDashboardStoriesState;
}

interface HomeDashboardSources extends Omit<HomeDashboardProps, "channelUnreadMap"> {
  channelUnreadMap: ChannelUnreadMap;
  mentionedRoomJids: Record<string, number>;
}

export function buildHomeDashboardProps(sources: HomeDashboardSources): HomeDashboardProps {
  return {
    spaces: sources.spaces,
    channels: sources.channels,
    contacts: sources.contacts,
    isLoading: sources.isLoading,
    channelUnreadMap: buildHomeChannelUnreadMap(
      sources.channels,
      sources.channelUnreadMap,
      sources.mentionedRoomJids,
    ),
    activeChannelJids: sources.activeChannelJids,
    dmConversations: sources.dmConversations,
    ...(sources.feed ? { feed: sources.feed } : {}),
    ...(sources.stories ? { stories: sources.stories } : {}),
  };
}

export function buildHomeChannelUnreadMap(
  channels: readonly Pick<ChannelSummary, "id" | "jid">[],
  unreadMap: ChannelUnreadMap,
  mentionedRoomJids: Record<string, number>,
): ChannelUnreadMap {
  const map: ChannelUnreadMap = {};
  for (const channel of channels) {
    const current = unreadMap[channel.id] ?? { unread: 0, mentions: 0 };
    const mentions = channel.jid ? mentionedRoomJids[bareJid(channel.jid)] ?? 0 : 0;
    map[channel.id] = {
      ...current,
      mentions: Math.max(current.mentions, mentions),
    };
  }
  return map;
}

function bareJid(jid: string): string {
  return jid.split("/")[0] ?? jid;
}
