import type { ChannelSummary } from "@/lib/chat-types";
import { normalizeMucCallRoomJid } from "./muc-call-presence";

export function normalizeMucServiceDomain(serviceJid?: string | null): string {
  const bare = serviceJid?.split("/")[0]?.trim().toLowerCase() ?? "";
  if (!bare) return "";
  return bare.includes("@") ? bare.split("@")[1] ?? "" : bare;
}

export function candidateRoomJidsForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  activeChannelJids: Iterable<string>,
  managedMucDomain?: string | null,
): string[] {
  const candidates: string[] = [];
  const channelRoomJid = normalizeMucCallRoomJid(channel.jid ?? "");
  const channelRoomDomain = channelRoomJid.split("@")[1] ?? "";
  const trustedDomain = channelRoomDomain || normalizeMucServiceDomain(managedMucDomain);
  if (channelRoomJid) candidates.push(channelRoomJid);
  if (!channelRoomJid && trustedDomain) {
    candidates.push(`${channel.id.toLowerCase()}@${trustedDomain}`);
  }
  for (const jid of activeChannelJids) {
    const normalized = normalizeMucCallRoomJid(jid);
    if (!normalized) continue;
    const localpart = normalized.split("@")[0] ?? "";
    const domain = normalized.split("@")[1] ?? "";
    if (trustedDomain && domain === trustedDomain && localpart === channel.id.toLowerCase()) {
      candidates.push(normalized);
    }
  }
  return [...new Set(candidates)];
}

export function callParticipantCountForChannel(
  channel: Pick<ChannelSummary, "id" | "jid">,
  callParticipantCounts: Record<string, number> | undefined,
  activeChannelJids: Iterable<string>,
  managedMucDomain?: string | null,
): number {
  if (!callParticipantCounts) return 0;
  for (const jid of candidateRoomJidsForChannel(channel, activeChannelJids, managedMucDomain)) {
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
