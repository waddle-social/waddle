/**
 * Author → real bare JID resolution for avatars.
 *
 * Sources, in order: our own JID for self-authored rows, the XEP-0313 MUC
 * archive real JID (`authorRealJid`), the occupant's real JID from live
 * XEP-0045 presence (`muc_jid`) — retained after the occupant leaves so
 * history keeps its faces — and, for 1:1 chats, the peer's own JID.
 * There is no `nick@domain` guessing: an unresolved author renders
 * initials, because initials beat a wrong face.
 *
 * XEP-0421 occupant ids are not surfaced by the client yet, so the
 * retained mapping is keyed by room + nick.
 */
import { shallowReactive } from "vue";
import { barePeerJid, resourceOf } from "@/lib/xmpp/jid";
import type { TimelineMessage } from "@/lib/chat-ui";

export type AuthorRef = Pick<TimelineMessage, "authorJid" | "authorOccupantJid" | "authorRealJid" | "isSelf">;

function bare(jid: string | null | undefined): string | null {
  if (!jid || !jid.includes("@")) return null;
  return barePeerJid(jid).toLowerCase() || null;
}

function occupantKey(roomJid: string, nick: string): string {
  return `${barePeerJid(roomJid).toLowerCase()}/${nick}`;
}

/** Last-known real JID per room occupant (room + nick), kept across leaves. */
export class OccupantJidDirectory {
  private readonly realJids = shallowReactive(new Map<string, string>());

  record(roomJid: string, nick: string, realJid: string): void {
    const real = bare(realJid);
    if (!roomJid || !nick || !real) return;
    const key = occupantKey(roomJid, nick);
    if (this.realJids.get(key) !== real) this.realJids.set(key, real);
  }

  /** Reactive lookup. */
  lookup(roomJid: string | null | undefined, nick: string | null | undefined): string | null {
    if (!roomJid || !nick) return null;
    return this.realJids.get(occupantKey(roomJid, nick)) ?? null;
  }

  clear(): void {
    this.realJids.clear();
  }
}

/** Real bare JID of a room occupant JID (`room@service/nick`), if known. */
function occupantRealJid(directory: OccupantJidDirectory, occupantJid: string): string | null {
  return directory.lookup(barePeerJid(occupantJid), resourceOf(occupantJid));
}

/**
 * Resolve a timeline/thread/search author to the bare JID whose avatar to
 * show, or `null` for initials.
 */
export function resolveAuthorJid(
  author: AuthorRef,
  directory: OccupantJidDirectory,
  selfJid?: string | null,
): string | null {
  if (author.isSelf && selfJid) return bare(selfJid);
  const real = bare(author.authorRealJid);
  if (real) return real;
  // Room rows (and MUC private messages) carry the occupant JID: only the
  // room's disclosure of the real JID may name the person behind a nick.
  if (author.authorOccupantJid) return occupantRealJid(directory, author.authorOccupantJid);
  // 1:1 rows: the sender's own JID.
  return bare(author.authorJid);
}

/** Process-wide retained occupant mapping fed by MUC presence. */
export const occupantJidDirectory = new OccupantJidDirectory();

/** {@link resolveAuthorJid} against the process-wide directory. */
export function authorAvatarJid(author: AuthorRef, selfJid?: string | null): string | null {
  return resolveAuthorJid(author, occupantJidDirectory, selfJid);
}

/** Real JID behind `nick` in `roomJid`, from the process-wide directory. */
export function roomOccupantAvatarJid(roomJid: string | null | undefined, nick: string): string | null {
  return occupantJidDirectory.lookup(roomJid, nick);
}

/**
 * Avatar JID for a 1:1 conversation peer: the bare JID of a direct chat,
 * or the room's disclosure for a MUC private-message occupant
 * (`room@service/nick`).
 */
export function conversationPeerAvatarJid(peerJid: string): string | null {
  if (resourceOf(peerJid)) return occupantRealJid(occupantJidDirectory, peerJid);
  return bare(peerJid);
}
