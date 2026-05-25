import type { ChannelSummary } from "@/lib/chat-types";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

export function candidateRoomJidsForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  activeChannelJids: Iterable<string>,
): string[] {
  const candidates: string[] = [];
  if (channel.jid) candidates.push(channel.jid);
  for (const jid of activeChannelJids) {
    const normalized = normalizeMucCallRoomJid(jid);
    if (!normalized) continue;
    const localpart = normalized.split("@")[0] ?? "";
    if (localpart === channel.id.toLowerCase()) {
      candidates.push(normalized);
    }
  }
  return candidates;
}

export function callParticipantCountForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  callParticipantCounts: Record<string, number> | undefined,
  activeChannelJids: Iterable<string>,
): number {
  if (!callParticipantCounts) return 0;
  for (const jid of candidateRoomJidsForChannel(channel, activeChannelJids)) {
    const count = callParticipantCounts[normalizeMucCallRoomJid(jid)] ?? 0;
    if (count > 0) return count;
  }
  return 0;
}

type CollapsedGroupActivitySummary = {
  unread: number;
  mentions: number;
  hasActivity: boolean;
  callCount: number;
};

type CollapsedGroupBadgeModel = {
  callCount: number;
  notification:
    | { kind: "mentions"; count: number }
    | { kind: "unread"; count: number }
    | { kind: "activity" }
    | null;
};

export function collapsedGroupBadgeModel(
  summary: CollapsedGroupActivitySummary,
): CollapsedGroupBadgeModel {
  return {
    callCount: summary.callCount > 0 ? summary.callCount : 0,
    notification:
      summary.mentions > 0
        ? { kind: "mentions", count: summary.mentions }
        : summary.unread > 0
          ? { kind: "unread", count: summary.unread }
          : summary.hasActivity
            ? { kind: "activity" }
            : null,
  };
}
