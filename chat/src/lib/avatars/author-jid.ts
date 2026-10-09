/**
 * Author → real bare JID resolution for avatars.
 *
 * A room row's avatar comes only from identity the row itself carries:
 * the archive's disclosed real JID (`authorRealJid`), or the stamp
 * (`authorAvatarJid`) put on it at arrival — an undelayed live row is
 * pinned to the nick's current disclosed holder, our own reflections and
 * local echoes to our JID. Anything else renders initials: a nick is
 * never resolved later against whoever holds it then, and no timestamps
 * or clocks are compared. 1:1 rows use the sender's own JID.
 *
 * Live occupant surfaces (typing, presence stacks, call tiles) use the
 * current holder of a nick, tracked from XEP-0045 presence. XEP-0421
 * occupant ids are not surfaced by the client yet, so it is keyed by
 * room + nick.
 */
import { shallowReactive } from "vue";
import { barePeerJid, resourceOf } from "@/lib/xmpp/jid";
import type { TimelineMessage } from "@/lib/chat-ui";

export type AuthorRef = Partial<Pick<
  TimelineMessage,
  "authorJid" | "authorOccupantJid" | "authorRealJid" | "authorAvatarJid" | "isSelf" | "createdAt" | "createdAtSource" | "deliveryStatus" | "archiveId"
>>;

function bare(jid: string | null | undefined): string | null {
  if (!jid?.includes("@")) return null;
  return barePeerJid(jid).toLowerCase() || null;
}

function occupantKey(roomJid: string, nick: string): string {
  return `${barePeerJid(roomJid).toLowerCase()}/${nick}`;
}

/**
 * The current disclosed holder of each room nick, plus our own actual
 * occupant nick per room. Departures do not forget a holder (their
 * already-stamped rows are unaffected either way); a JID-less holder
 * taking the nick does.
 */
export class OccupantJidDirectory {
  private readonly holders = shallowReactive(new Map<string, string>());
  /** Our actual occupant nick per room (XEP-0045 self-presence; 210 may rename us). */
  private readonly ownNicks = shallowReactive(new Map<string, string>());
  /**
   * Bare JIDs the server vouches for as bots: seen carrying its bot hat in
   * any room, or listed in a room's declared bot list. A bot stays one for
   * the session even after it leaves a room.
   */
  private readonly bots = shallowReactive(new Set<string>());

  /**
   * Record who holds `nick` now. `realJid` null means an occupant whose
   * real JID is not disclosed to us: the previous holder no longer names
   * the nick. `isBot`: the occupant's presence carried the bot hat.
   */
  record(roomJid: string, nick: string, realJid: string | null, isBot = false): void {
    if (!roomJid || !nick) return;
    const key = occupantKey(roomJid, nick);
    const real = realJid === null ? null : bare(realJid);
    if (real) {
      if (isBot) this.bots.add(real);
      if (this.holders.get(key) !== real) this.holders.set(key, real);
    } else if (realJid === null) {
      this.holders.delete(key);
    }
  }

  /** Reactive: the disclosed real JID currently behind `nick`, if any. */
  lookup(roomJid: string | null | undefined, nick: string | null | undefined): string | null {
    if (!roomJid || !nick) return null;
    return this.holders.get(occupantKey(roomJid, nick)) ?? null;
  }

  /** Reactive: `jid` is a known server-hosted bot. */
  isBot(jid: string | null | undefined): boolean {
    const real = bare(jid);
    return !!real && this.bots.has(real);
  }

  /** Record the bots a room's server-declared bot list names (XEP-0030 `urn:waddle:room:bots:0`). */
  recordBots(jids: Iterable<string>): void {
    for (const jid of jids) {
      const real = bare(jid);
      if (real) this.bots.add(real);
    }
  }

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
   * own nicks until the new self-presence (110) re-records them.
   */
  forgetOwnNicks(): void {
    this.ownNicks.clear();
  }

  clear(): void {
    this.holders.clear();
    this.ownNicks.clear();
    this.bots.clear();
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
  // At render time only a local echo proves a room row is ours: the live
  // own-nick check runs once, at ingest (stampLiveRoomAuthor), because we
  // may later take a nick an unstamped row was sent under.
  const localEcho = !!author.isSelf && author.deliveryStatus !== undefined;
  const ownDirect = !author.authorOccupantJid && isOwnSend(author, directory);
  if (selfJid && (localEcho || ownDirect)) return bare(selfJid);
  // Any other room row (or MUC private message) carries only a nick, which
  // may have changed hands: initials rather than a possibly wrong face.
  if (author.authorOccupantJid) return null;
  // 1:1 rows: the sender's own JID.
  return bare(author.authorJid);
}

/** Process-wide occupant directory fed by MUC presence. */
export const occupantJidDirectory = new OccupantJidDirectory();

/** Reactive: `jid` is a known server-hosted bot, which cannot take direct messages. */
export function isBotJid(jid: string | null | undefined): boolean {
  return occupantJidDirectory.isBot(jid);
}

/** {@link resolveAuthorJid} against the process-wide directory. */
export function authorAvatarJid(author: AuthorRef, selfJid?: string | null): string | null {
  return resolveAuthorJid(author, occupantJidDirectory, selfJid);
}

/** Real JID currently behind `nick` in `roomJid` (live occupant surfaces). */
export function roomOccupantAvatarJid(roomJid: string | null | undefined, nick: string): string | null {
  return occupantJidDirectory.lookup(roomJid, nick);
}

/**
 * Pin a row delivered on the live path to the person behind its nick, so
 * a later reuse of the nick cannot change whose face (and profile) it
 * shows. Only an undelayed row is pinned to the current holder; a delayed
 * or archive-stamped row (SM replay, MUC history, catch-up re-emission)
 * was sent at some past moment whose holder we cannot know, so it keeps
 * only the identity it carries (initials otherwise).
 */
export function stampLiveRoomAuthor<T extends AuthorRef>(row: T, roomJid: string, nick: string, selfJid?: string | null): T {
  // Live groupchat echoes need not disclose muc#user item@jid. Our actual
  // available self-presence proves this occupant is our own account; a
  // configured nick, avatar stamp, or delivery status cannot provide it.
  const self = bare(selfJid);
  if (!row.authorRealJid
    && row.createdAtSource === "fallback"
    && row.archiveId === undefined
    && row.authorOccupantJid === `${roomJid}/${nick}`
    && occupantJidDirectory.ownNick(roomJid) === nick
    && self && /^[^\s@]+@[^\s@]+$/.test(self)
  ) {
    return { ...row, authorRealJid: self, authorAvatarJid: row.authorAvatarJid ?? self, isSelf: true };
  }
  if (row.authorRealJid || row.authorAvatarJid) return row;
  if (row.archiveId !== undefined) return row;
  if (isOwnSend(row, occupantJidDirectory)) {
    // Pin our own reflection now, while our actual nick is known: the
    // own-nick map is forgotten on a fresh session.
    const self = bare(selfJid);
    return self ? { ...row, authorAvatarJid: self } : row;
  }
  if (row.createdAtSource === "archive" || row.createdAtSource === "delay") return row;
  const author = occupantJidDirectory.lookup(roomJid, nick);
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

