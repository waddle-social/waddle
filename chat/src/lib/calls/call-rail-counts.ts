import { barePeerJid } from "@/lib/xmpp/jid";
import type { DmCallActivity } from "./dm-call-activity";
import { normalizeMucCallRoomJid } from "./muc-call-presence";
import type { CallState } from "./types";

export function activeChannelRailCallCount(
  callParticipantCounts: Record<string, number>,
  callState: CallState,
): number {
  const rooms = new Set(
    Object.entries(callParticipantCounts)
      .filter(([, count]) => count > 0)
      .map(([roomJid]) => normalizeMucCallRoomJid(roomJid))
      .filter(Boolean),
  );
  if ((callState.phase === "muc-pending" || callState.phase === "active") && callState.kind === "muc") {
    const roomJid = normalizeMucCallRoomJid(callState.peer);
    if (roomJid) rooms.add(roomJid);
  }
  return rooms.size;
}

export function activeDmRailCallCount(
  dmCallActivities: Record<string, DmCallActivity>,
  callState: CallState,
): number {
  const peers = new Set(
    Object.values(dmCallActivities)
      .map((activity) => barePeerJid(activity.peerJid).toLowerCase())
      .filter(Boolean),
  );
  const currentPeer = currentDmCallPeerJid(callState);
  if (currentPeer) peers.add(currentPeer);
  return peers.size;
}

function currentDmCallPeerJid(callState: CallState): string {
  if (callState.phase === "active" && callState.kind === "dm") {
    return barePeerJid(callState.peer).toLowerCase();
  }
  if (callState.phase === "incoming") return barePeerJid(callState.from).toLowerCase();
  if (callState.phase === "outgoing") return barePeerJid(callState.to).toLowerCase();
  return "";
}
