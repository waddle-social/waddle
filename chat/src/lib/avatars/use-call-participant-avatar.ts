import { useStore } from "@nanostores/vue";
import { $mucCallParticipantOwners, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { callParticipantAvatarJid } from "./author-jid";

/**
 * Reactive resolver for MUC call participant nicks: the Muji owner's real
 * JID from call presence when known, else the room's current disclosure.
 */
export function useCallParticipantAvatarJid(): (roomJid: string, nick: string) => string | null {
  const owners = useStore($mucCallParticipantOwners);
  return (roomJid, nick) =>
    callParticipantAvatarJid(roomJid, nick, owners.value[normalizeMucCallRoomJid(roomJid)] ?? []);
}
