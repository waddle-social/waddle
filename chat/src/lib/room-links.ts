import type { InjectionKey } from "vue";
import type { ChannelSummary } from "@/lib/chat-types";

const ROOM_JOIN_URI = /^xmpp:((?:[a-z0-9._~!$()*+,;=-]|%[0-9a-f]{2})+)@([a-z0-9.-]+|\[[0-9a-f:.]+\])\?join$/i;

export function roomJidFromJoinUri(uri: string): string | null {
  const match = ROOM_JOIN_URI.exec(uri);
  if (!match) return null;
  try {
    const localpart = decodeURIComponent(match[1]);
    if (/(?:\p{Cc}|\s|["&'/:<>@])/u.test(localpart)) return null;
    return `${localpart}@${match[2].toLowerCase()}`;
  } catch {
    return null;
  }
}

export interface RoomLinkCandidate {
  jid: string;
  uri: string;
  label: string;
}

export function roomLinkCandidates(channels: readonly ChannelSummary[]): RoomLinkCandidate[] {
  const rooms = new Map<string, RoomLinkCandidate>();
  for (const channel of channels) {
    if (!channel.jid || channel.isGroupDm) continue;
    const at = channel.jid.lastIndexOf("@");
    if (at <= 0) continue;
    const uri = `xmpp:${encodeURIComponent(channel.jid.slice(0, at))}${channel.jid.slice(at)}?join`;
    const jid = roomJidFromJoinUri(uri);
    if (jid) rooms.set(jid, { jid, uri, label: `#${channel.name || channel.id}` });
  }
  return [...rooms.values()];
}

export function searchRoomLinks(rooms: readonly RoomLinkCandidate[], text: string): RoomLinkCandidate[] {
  const query = text.toLowerCase();
  const exact: RoomLinkCandidate[] = [];
  const partial: RoomLinkCandidate[] = [];
  for (const room of rooms) {
    const name = room.label.slice(1).toLowerCase();
    const localpart = room.jid.split("@")[0].toLowerCase();
    if (name === query || localpart === query) exact.push(room);
    else if (name.includes(query) || localpart.includes(query)) partial.push(room);
  }
  return [...exact, ...partial].slice(0, 8);
}

export const roomLinksKey: InjectionKey<{
  candidates: () => RoomLinkCandidate[];
  href: (roomJid: string) => string | null;
  open: (roomJid: string) => void;
}> = Symbol("room-links");
