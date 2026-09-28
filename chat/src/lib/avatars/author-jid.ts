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

export type AuthorRef = Partial<Pick<
  TimelineMessage,
  "authorJid" | "authorOccupantJid" | "authorRealJid" | "authorAvatarJid" | "isSelf" | "createdAt" | "createdAtSource" | "deliveryStatus"
>>;

function bare(jid: string | null | undefined): string | null {
  if (!jid || !jid.includes("@")) return null;
  return barePeerJid(jid).toLowerCase() || null;
}

function occupantKey(roomJid: string, nick: string): string {
  return `${barePeerJid(roomJid).toLowerCase()}/${nick}`;
}

interface OccupantMapping {
  /** `null`: the nick was held by an occupant whose real JID we cannot see. */
  realJid: string | null;
  /** Local clock when this holder was first seen behind the nick. */
  since: number;
}

/**
 * Real JIDs seen behind each room occupant (room + nick), kept across
 * leaves. A nick can be reused by someone else, so the directory keeps
 * the history: live occupant surfaces read the current mapping, while a
 * past row may only use the mapping that was in effect when it was sent.
 */
export class OccupantJidDirectory {
  private readonly history = shallowReactive(new Map<string, readonly OccupantMapping[]>());

  constructor(private readonly now: () => number = () => Date.now()) {}

  /**
   * Record who holds `nick` from now on. `realJid` null means an occupant
   * whose real JID is not disclosed: the nick's previous holder stops being
   * its current holder, while earlier rows keep their earlier mapping.
   * Departures are never recorded; they do not change who sent past rows.
   */
  record(roomJid: string, nick: string, realJid: string | null): void {
    if (!roomJid || !nick) return;
    const real = realJid === null ? null : bare(realJid);
    if (realJid !== null && !real) return;
    const key = occupantKey(roomJid, nick);
    const previous = this.history.get(key) ?? [];
    const last = previous.at(-1);
    if (last ? last.realJid === real : real === null) return;
    this.history.set(key, [...previous, { realJid: real, since: this.now() }]);
  }

  /** Reactive: who is behind `nick` right now (the latest mapping). */
  lookup(roomJid: string | null | undefined, nick: string | null | undefined): string | null {
    if (!roomJid || !nick) return null;
    return this.history.get(occupantKey(roomJid, nick))?.at(-1)?.realJid ?? null;
  }

  /**
   * Reactive: who was behind `nick` at `atMs`. A mapping first seen after
   * that instant never re-attributes an earlier row; no mapping yet means
   * `null` (initials).
   */
  lookupAt(roomJid: string | null | undefined, nick: string | null | undefined, atMs: number): string | null {
    if (!roomJid || !nick || !Number.isFinite(atMs)) return null;
    const mappings = this.history.get(occupantKey(roomJid, nick)) ?? [];
    for (let i = mappings.length - 1; i >= 0; i -= 1) {
      const mapping = mappings[i]!;
      if (mapping.since <= atMs) return mapping.realJid;
    }
    return null;
  }

  /** Our actual occupant nick per room (XEP-0045 self-presence; 210 may rename us). */
  private readonly ownNicks = shallowReactive(new Map<string, string>());

  recordOwnNick(roomJid: string, nick: string | null): void {
    const room = barePeerJid(roomJid).toLowerCase();
    if (!room) return;
    if (nick) this.ownNicks.set(room, nick);
    else this.ownNicks.delete(room);
  }

  /** Reactive: our current occupant nick in `roomJid`, if joined. */
  ownNick(roomJid: string): string | null {
    return this.ownNicks.get(barePeerJid(roomJid).toLowerCase()) ?? null;
  }

  /**
   * A fresh session (no stream resumption) must rejoin every room, and a
   * nick we held may have changed hands while we were offline: forget our
   * own nicks until the new self-presence (110) re-records them. Author
   * history is kept for time-scoped lookups.
   */
  forgetOwnNicks(): void {
    this.ownNicks.clear();
  }

  clear(): void {
    this.history.clear();
    this.ownNicks.clear();
  }
}

/** Current real bare JID of a room occupant JID (`room@service/nick`), if known. */
function occupantRealJid(directory: OccupantJidDirectory, occupantJid: string): string | null {
  return directory.lookup(barePeerJid(occupantJid), resourceOf(occupantJid));
}

/**
 * A row this client sent (a local echo carries a delivery status) or saw
 * reflected live under our ACTUAL occupant nick in that room. A room row's
 * `isSelf` compares against the nick we asked for, which a peer may hold
 * while the room assigned us another (XEP-0045 210), so it never names us
 * on its own; neither does a past holder of our nick.
 */
function isOwnSend(author: AuthorRef, directory: OccupantJidDirectory): boolean {
  if (author.isSelf && author.deliveryStatus !== undefined) return true;
  const live = author.createdAtSource === "fallback" || author.createdAtSource === "queued";
  if (!live) return false;
  if (author.authorOccupantJid) {
    const ownNick = directory.ownNick(barePeerJid(author.authorOccupantJid));
    return !!ownNick && resourceOf(author.authorOccupantJid) === ownNick;
  }
  return !!author.isSelf;
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
  // A disclosed identity always wins: the archive's real JID, then the
  // occupant JID stamped at live ingest. Only then our own sends.
  const real = bare(author.authorRealJid);
  if (real) return real;
  const stamped = bare(author.authorAvatarJid);
  if (stamped) return stamped;
  if (selfJid && isOwnSend(author, directory)) return bare(selfJid);
  // Other room rows (and MUC private messages) carry only the occupant
  // JID: use the room's disclosure that was in effect when the row was
  // sent, so a later reuse of the nick never re-attributes it.
  if (author.authorOccupantJid) {
    return directory.lookupAt(
      barePeerJid(author.authorOccupantJid),
      resourceOf(author.authorOccupantJid),
      Date.parse(author.createdAt ?? ""),
    );
  }
  // 1:1 rows: the sender's own JID.
  return bare(author.authorJid);
}

/** Process-wide retained occupant mapping fed by MUC presence. */
export const occupantJidDirectory = new OccupantJidDirectory();

/** {@link resolveAuthorJid} against the process-wide directory. */
export function authorAvatarJid(author: AuthorRef, selfJid?: string | null): string | null {
  return resolveAuthorJid(author, occupantJidDirectory, selfJid);
}

/** Real JID currently behind `nick` in `roomJid` (live occupant surfaces). */
export function roomOccupantAvatarJid(roomJid: string | null | undefined, nick: string): string | null {
  return occupantJidDirectory.lookup(roomJid, nick);
}

/**
 * Pin a room row delivered on the live path to the person behind its nick,
 * so a later reuse of the nick cannot change whose face (and profile) it
 * shows. An undelayed row is pinned to the current occupant; a delayed or
 * archive-stamped row (SM replay, MUC history, catch-up re-emission) was
 * sent in the past, so only the mapping in effect at its timestamp may
 * name it.
 */
export function stampLiveRoomAuthor<T extends AuthorRef>(row: T, roomJid: string, nick: string): T {
  if (isOwnSend(row, occupantJidDirectory) || row.authorRealJid || row.authorAvatarJid) return row;
  const past = row.createdAtSource === "archive" || row.createdAtSource === "delay";
  const author = past
    ? occupantJidDirectory.lookupAt(roomJid, nick, Date.parse(row.createdAt ?? ""))
    : occupantJidDirectory.lookup(roomJid, nick);
  return author ? { ...row, authorAvatarJid: author } : row;
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

/**
 * Avatar JID for a MUC call participant nick: the Muji owner's real JID
 * when the call presence carried one, else the room's disclosure.
 */
export function callParticipantAvatarJid(
  roomJid: string,
  nick: string,
  owners: readonly { nick: string; realJid?: string }[],
): string | null {
  const owner = owners.find((entry) => entry.nick === nick);
  return owner?.realJid ? bare(owner.realJid) : roomOccupantAvatarJid(roomJid, nick);
}

/**
 * Avatar JID for a typing notification's nick. A 1:1 chat's typer is the
 * peer, whatever their display name (a peer's localpart may equal ours on
 * another domain); a room's typer is the nick's current, disclosed holder.
 * Nick equality with our own name never makes it us.
 */
export function typingAuthorAvatarJid(
  nick: string,
  context: { peerJid: string } | { roomJid: string | null | undefined },
): string | null {
  if ("peerJid" in context) return conversationPeerAvatarJid(context.peerJid);
  return roomOccupantAvatarJid(context.roomJid, nick);
}

