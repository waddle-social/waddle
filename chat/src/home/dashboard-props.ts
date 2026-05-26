import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { RosterContact } from "@/lib/xmpp/types";
import type { DmConversation } from "@/lib/xmpp-client";
import type { ChannelUnreadMap } from "@/home/activity";
import type { DmCallActivity } from "@/lib/calls/dm-call-activity";

export interface HomeDashboardProps {
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  contacts: RosterContact[];
  isLoading: boolean;
  channelUnreadMap?: ChannelUnreadMap;
  activeChannelJids?: Set<string>;
  dmConversations?: DmConversation[];
  callParticipantCounts?: Record<string, number>;
  dmCallActivities?: Record<string, DmCallActivity>;
  managedMucDomain?: string | null;
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
    callParticipantCounts: sources.callParticipantCounts,
    dmCallActivities: sources.dmCallActivities,
    managedMucDomain: sources.managedMucDomain,
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
