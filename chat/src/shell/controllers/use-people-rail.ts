import { computed, type ComputedRef, type Ref } from "vue";
import type { MemberSummary } from "@/lib/chat-types";
import type { DmConversation, OccupantPresence, RoomPresence, RosterContact } from "@/lib/xmpp/types";
import type { DmCallActivity } from "@/lib/calls/dm-call-activity";
import { barePeerJid } from "@/lib/xmpp/jid";

/**
 * People rail model: the one place that turns the controller's disjoint
 * presence sources (nick-keyed room occupants, bare-JID roster contacts and
 * DM peers, Muji call participants, LiveKit active speakers) into a single
 * bare-JID keyed list of people grouped the way the Huddle shell shows them.
 *
 * Everything here is honest about what the client can actually observe:
 * "speaking" exists only for the call this client is connected to, room
 * occupants exist only for the focused room, and nobody has a join date.
 */

type PeopleRailStatus = "speaking" | "in-huddle" | "available" | "away" | "dnd" | "offline";

export interface PeopleRailPerson {
  /** Bare JID — the dedupe key across every source. */
  jid: string;
  name: string;
  avatarUrl: string | null;
  /** Presence dot for `AppAvatar`; undefined when no presence is known. */
  presence: OccupantPresence | undefined;
  status: PeopleRailStatus;
  /** Short truthful status line ("speaking", "in the huddle", "away"). */
  statusText: string | null;
  /** XEP-0108 in-call overlay (any call, not only ours). */
  inCall: boolean;
}

export interface MemberCardModel extends PeopleRailPerson {
  affiliation: MemberSummary["affiliation"] | null;
}

export interface PeopleRailGroups {
  /** Members of the active room's call, plus DM peers in a call with us. */
  huddle: PeopleRailPerson[];
  /** Occupants of the focused room (only when a room is active). */
  room: PeopleRailPerson[];
  /** Roster contacts and DM peers who are available or busy. */
  around: PeopleRailPerson[];
  /** Roster contacts and DM peers who are away, offline, or unknown. */
  awayAndOffline: PeopleRailPerson[];
}

export interface PeopleRailSources {
  selfJid: string | null;
  /** Focused-room occupant presence keyed by nick (XEP-0045). */
  roomPresence: RoomPresence;
  /** nick -> bare JID for the focused room. */
  authorJidByNick: Record<string, string>;
  /** nick -> avatar URL for the focused room (lazy fetch). */
  avatarUrlByAuthor: Record<string, string | null>;
  /** Affiliation list merged with online occupants, avatars resolved. */
  members: readonly MemberSummary[];
  /** Bare room JID of the focused room, or null. */
  activeRoomJid: string | null;
  /** Muji participant nicks keyed by room JID. */
  callParticipants: Record<string, readonly string[]>;
  /** Muji participant owners keyed by room JID (realJid when known). */
  callParticipantOwners: Record<string, readonly { nick: string; realJid?: string }[]>;
  /** Room JID of the MUC call this client is connected to, or null. */
  ownCallRoomJid: string | null;
  /** Bare JIDs of LiveKit active speakers in our own call. */
  speakingJids: ReadonlySet<string>;
  dmCallActivities: Record<string, DmCallActivity>;
  contacts: readonly RosterContact[];
  conversations: readonly DmConversation[];
  peerInCall: (jid: string) => boolean;
}

type PresenceShow = DmConversation["presenceShow"];

const STATUS_ORDER: Record<PeopleRailStatus, number> = {
  speaking: 0,
  "in-huddle": 1,
  available: 2,
  away: 3,
  dnd: 4,
  offline: 5,
};

export function presenceFromShow(show: PresenceShow): OccupantPresence | undefined {
  switch (show) {
    case "available":
      return "online";
    case "away":
    case "xa":
      return "away";
    case "dnd":
      return "dnd";
    case "offline":
      return "offline";
    default:
      return undefined;
  }
}

/**
 * The one rule for "around": available or do-not-disturb. Away, extended
 * away, offline and unknown are not. The Home hero, the Home "Around"
 * section, the context column, the people rail and the Members "Here
 * now" filter all use it so one screen never disagrees with itself.
 */
export function isAroundPresence(presence: OccupantPresence | undefined): boolean {
  return presence === "online" || presence === "dnd";
}

interface KnownPerson {
  /** Bare JID — the dedupe key across roster and DM peers. */
  jid: string;
  name: string;
  avatarUrl: string | null;
  presence: OccupantPresence | undefined;
  /** The raw 1:1 show, for copy such as `dmPresenceLabel`. */
  presenceShow: PresenceShow;
}

export interface KnownPeopleGroups {
  around: KnownPerson[];
  awayAndOffline: KnownPerson[];
}

/**
 * Roster contacts and DM peers merged by bare JID (MUC private messages
 * excluded, since a nick is not a person we know by JID), split by
 * `isAroundPresence`. A DM conversation's presence wins over the roster's,
 * a roster name over a DM username, matching the people rail.
 */
export function splitKnownPeople(
  contacts: readonly RosterContact[],
  conversations: readonly DmConversation[],
): KnownPeopleGroups {
  const contactByJid = new Map<string, RosterContact>();
  for (const contact of contacts) contactByJid.set(bare(contact.jid), contact);
  const conversationByJid = new Map<string, DmConversation>();
  for (const conversation of conversations) {
    if (conversation.mucPm) continue;
    conversationByJid.set(bare(conversation.peerJid), conversation);
  }
  const around: KnownPerson[] = [];
  const awayAndOffline: KnownPerson[] = [];
  for (const jid of new Set<string>([...contactByJid.keys(), ...conversationByJid.keys()])) {
    if (!jid) continue;
    const contact = contactByJid.get(jid);
    const conversation = conversationByJid.get(jid);
    const presenceShow = conversation?.presenceShow ?? contact?.presenceShow;
    const person: KnownPerson = {
      jid,
      name: contact?.name || conversation?.peerUsername || contact?.username || jid,
      avatarUrl: conversation?.peerAvatarUrl ?? null,
      presence: presenceFromShow(presenceShow),
      presenceShow,
    };
    if (isAroundPresence(person.presence)) around.push(person);
    else awayAndOffline.push(person);
  }
  around.sort(compareKnownPeople);
  awayAndOffline.sort(compareKnownPeople);
  return { around, awayAndOffline };
}

function compareKnownPeople(a: KnownPerson, b: KnownPerson): number {
  return (
    STATUS_ORDER[statusFromPresence(a.presence)] - STATUS_ORDER[statusFromPresence(b.presence)]
    || a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
    || a.jid.localeCompare(b.jid)
  );
}

function statusFromPresence(presence: OccupantPresence | undefined): PeopleRailStatus {
  switch (presence) {
    case "online":
      return "available";
    case "away":
      return "away";
    case "dnd":
      return "dnd";
    default:
      return "offline";
  }
}

function statusText(status: PeopleRailStatus): string | null {
  switch (status) {
    case "speaking":
      return "speaking";
    case "in-huddle":
      return "in the huddle";
    case "available":
      return "available";
    case "away":
      return "away";
    case "dnd":
      return "do not disturb";
    default:
      return null;
  }
}

function comparePeople(a: PeopleRailPerson, b: PeopleRailPerson): number {
  return (
    STATUS_ORDER[a.status] - STATUS_ORDER[b.status]
    || a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
    || a.jid.localeCompare(b.jid)
  );
}

function bare(jid: string): string {
  return barePeerJid(jid).toLowerCase();
}

function memberAvatarIndex(members: readonly MemberSummary[]): Map<string, MemberSummary> {
  const index = new Map<string, MemberSummary>();
  for (const member of members) index.set(bare(member.jid), member);
  return index;
}

/** Pure derivation of the rail groups from a snapshot of every source. */
export function buildPeopleRail(sources: PeopleRailSources): PeopleRailGroups {
  const selfKey = sources.selfJid ? bare(sources.selfJid) : null;
  const seen = new Set<string>();
  const memberIndex = memberAvatarIndex(sources.members);
  const contactByJid = new Map<string, RosterContact>();
  for (const contact of sources.contacts) contactByJid.set(bare(contact.jid), contact);
  const conversationByJid = new Map<string, DmConversation>();
  for (const conversation of sources.conversations) {
    // MUC private messages address an occupant, not an account; never
    // list a nick as if it were a person we know by JID (#1256).
    if (conversation.mucPm) continue;
    conversationByJid.set(bare(conversation.peerJid), conversation);
  }

  function knownPresence(jid: string): OccupantPresence | undefined {
    const conversation = conversationByJid.get(jid);
    const fromDm = presenceFromShow(conversation?.presenceShow);
    if (fromDm) return fromDm;
    return presenceFromShow(contactByJid.get(jid)?.presenceShow);
  }

  function knownName(jid: string, fallback: string): string {
    const contact = contactByJid.get(jid);
    if (contact?.name) return contact.name;
    const conversation = conversationByJid.get(jid);
    if (conversation?.peerUsername) return conversation.peerUsername;
    if (contact?.username) return contact.username;
    return memberIndex.get(jid)?.username ?? fallback;
  }

  function knownAvatar(jid: string, nick?: string): string | null {
    if (nick && sources.avatarUrlByAuthor[nick]) return sources.avatarUrlByAuthor[nick] ?? null;
    return memberIndex.get(jid)?.avatar_url ?? conversationByJid.get(jid)?.peerAvatarUrl ?? null;
  }

  function claim(jid: string): boolean {
    if (!jid || jid === selfKey || seen.has(jid)) return false;
    seen.add(jid);
    return true;
  }

  // ── In a huddle ────────────────────────────────────────────────────
  const huddle: PeopleRailPerson[] = [];
  const huddleRooms = new Set<string>();
  if (sources.activeRoomJid) huddleRooms.add(bare(sources.activeRoomJid));
  if (sources.ownCallRoomJid) huddleRooms.add(bare(sources.ownCallRoomJid));
  for (const roomJid of huddleRooms) {
    const nicks = sources.callParticipants[roomJid] ?? [];
    const owners = sources.callParticipantOwners[roomJid] ?? [];
    const isActiveRoom = sources.activeRoomJid !== null && bare(sources.activeRoomJid) === roomJid;
    for (const nick of nicks) {
      const owner = owners.find((entry) => entry.nick === nick);
      const mapped = isActiveRoom ? sources.authorJidByNick[nick] : undefined;
      const realJid = mapped ?? owner?.realJid;
      if (!realJid) continue; // nick-only participants cannot be keyed honestly
      const jid = bare(realJid);
      if (!claim(jid)) continue;
      const speaking = sources.speakingJids.has(jid);
      const status: PeopleRailStatus = speaking ? "speaking" : "in-huddle";
      huddle.push({
        jid,
        name: knownName(jid, nick),
        avatarUrl: knownAvatar(jid, isActiveRoom ? nick : undefined),
        presence: (isActiveRoom ? sources.roomPresence[nick] : undefined) ?? knownPresence(jid) ?? "online",
        status,
        statusText: statusText(status),
        inCall: true,
      });
    }
  }
  for (const activity of Object.values(sources.dmCallActivities)) {
    if (activity.state !== "accepted") continue;
    const jid = bare(activity.peerJid);
    if (!claim(jid)) continue;
    const speaking = sources.speakingJids.has(jid);
    const status: PeopleRailStatus = speaking ? "speaking" : "in-huddle";
    huddle.push({
      jid,
      name: knownName(jid, activity.peerJid),
      avatarUrl: knownAvatar(jid),
      presence: knownPresence(jid) ?? "online",
      status,
      statusText: statusText(status),
      inCall: true,
    });
  }
  huddle.sort(comparePeople);

  // ── In this room ───────────────────────────────────────────────────
  const room: PeopleRailPerson[] = [];
  if (sources.activeRoomJid) {
    for (const [nick, occupantPresence] of Object.entries(sources.roomPresence)) {
      if (occupantPresence === "offline") continue;
      const realJid = sources.authorJidByNick[nick];
      if (!realJid) continue;
      const jid = bare(realJid);
      if (!claim(jid)) continue;
      const status = statusFromPresence(occupantPresence);
      room.push({
        jid,
        name: knownName(jid, nick),
        avatarUrl: knownAvatar(jid, nick),
        presence: occupantPresence,
        status,
        statusText: statusText(status),
        inCall: sources.peerInCall(jid),
      });
    }
    room.sort(comparePeople);
  }

  // ── Around / away and offline ──────────────────────────────────────
  const around: PeopleRailPerson[] = [];
  const awayAndOffline: PeopleRailPerson[] = [];
  const peopleJids = new Set<string>([...contactByJid.keys(), ...conversationByJid.keys()]);
  for (const jid of peopleJids) {
    if (!claim(jid)) continue;
    const presence = knownPresence(jid);
    const status = statusFromPresence(presence);
    const person: PeopleRailPerson = {
      jid,
      name: knownName(jid, jid),
      avatarUrl: knownAvatar(jid),
      presence,
      status,
      statusText: statusText(status),
      inCall: sources.peerInCall(jid),
    };
    if (isAroundPresence(presence)) around.push(person);
    else awayAndOffline.push(person);
  }
  around.sort(comparePeople);
  awayAndOffline.sort(comparePeople);

  return { huddle, room, around, awayAndOffline };
}

/** Case-insensitive client-side filter over name and JID. */
export function filterPeople(people: readonly PeopleRailPerson[], query: string): PeopleRailPerson[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [...people];
  return people.filter((person) =>
    person.name.toLowerCase().includes(needle) || person.jid.includes(needle),
  );
}

export interface MemberCardSources {
  /** True when a room is focused: cards come from its affiliation list. */
  roomActive: boolean;
  members: readonly MemberSummary[];
  roomPresence: RoomPresence;
  authorJidByNick: Record<string, string>;
  avatarUrlByAuthor: Record<string, string | null>;
  contacts: readonly RosterContact[];
  conversations: readonly DmConversation[];
  huddleJids: ReadonlySet<string>;
  speakingJids: ReadonlySet<string>;
  peerInCall: (jid: string) => boolean;
}

/**
 * Directory cards for the Members page. With a room focused the source is
 * the room's affiliation list (merged with online occupants); otherwise the
 * roster. Presence for room members comes from the nick-keyed occupant map
 * through the reverse of `authorJidByNick`.
 */
export function buildMemberCards(sources: MemberCardSources): MemberCardModel[] {
  const cards: MemberCardModel[] = [];
  const seen = new Set<string>();
  const nickByJid = new Map<string, string>();
  for (const [nick, jid] of Object.entries(sources.authorJidByNick)) nickByJid.set(bare(jid), nick);
  const contactByJid = new Map<string, RosterContact>();
  for (const contact of sources.contacts) contactByJid.set(bare(contact.jid), contact);
  const conversationByJid = new Map<string, DmConversation>();
  for (const conversation of sources.conversations) {
    if (conversation.mucPm) continue;
    conversationByJid.set(bare(conversation.peerJid), conversation);
  }

  function statusFor(jid: string, presence: OccupantPresence | undefined): PeopleRailStatus {
    if (sources.speakingJids.has(jid)) return "speaking";
    if (sources.huddleJids.has(jid)) return "in-huddle";
    return statusFromPresence(presence);
  }

  if (sources.roomActive) {
    for (const member of sources.members) {
      const jid = bare(member.jid);
      if (!jid || seen.has(jid)) continue;
      seen.add(jid);
      const nick = nickByJid.get(jid);
      const presence = (nick ? sources.roomPresence[nick] : undefined)
        ?? presenceFromShow(conversationByJid.get(jid)?.presenceShow)
        ?? presenceFromShow(contactByJid.get(jid)?.presenceShow);
      const status = statusFor(jid, presence);
      cards.push({
        jid,
        name: member.username || nick || jid,
        avatarUrl: (nick ? sources.avatarUrlByAuthor[nick] : null) ?? member.avatar_url ?? null,
        presence,
        status,
        statusText: statusText(status),
        inCall: sources.huddleJids.has(jid) || sources.peerInCall(jid),
        affiliation: member.affiliation,
      });
    }
  } else {
    const jids = new Set<string>([...contactByJid.keys(), ...conversationByJid.keys()]);
    for (const jid of jids) {
      if (seen.has(jid)) continue;
      seen.add(jid);
      const contact = contactByJid.get(jid);
      const conversation = conversationByJid.get(jid);
      const presence = presenceFromShow(conversation?.presenceShow) ?? presenceFromShow(contact?.presenceShow);
      const status = statusFor(jid, presence);
      cards.push({
        jid,
        name: contact?.name || conversation?.peerUsername || contact?.username || jid,
        avatarUrl: conversation?.peerAvatarUrl ?? null,
        presence,
        status,
        statusText: statusText(status),
        inCall: sources.huddleJids.has(jid) || sources.peerInCall(jid),
        affiliation: null,
      });
    }
  }
  cards.sort(comparePeople);
  return cards;
}

export interface PeopleRailDeps {
  selfJid: Ref<string | null> | ComputedRef<string | null>;
  roomPresence: Ref<RoomPresence>;
  authorJidByNick: Ref<Record<string, string>> | ComputedRef<Record<string, string>>;
  avatarUrlByAuthor: Ref<Record<string, string | null>> | ComputedRef<Record<string, string | null>>;
  members: Ref<readonly MemberSummary[]> | ComputedRef<readonly MemberSummary[]>;
  activeRoomJid: Ref<string | null> | ComputedRef<string | null>;
  callParticipants: Ref<Record<string, readonly string[]>> | ComputedRef<Record<string, readonly string[]>>;
  callParticipantOwners: Ref<Record<string, readonly { nick: string; realJid?: string }[]>>;
  ownCallRoomJid: Ref<string | null> | ComputedRef<string | null>;
  speakingJids: Ref<ReadonlySet<string>> | ComputedRef<ReadonlySet<string>>;
  dmCallActivities: Ref<Record<string, DmCallActivity>>;
  contacts: Ref<readonly RosterContact[]>;
  conversations: Ref<readonly DmConversation[]>;
  peerInCall: (jid: string) => boolean;
}

/** Reactive wrapper: recomputes the groups whenever any source changes. */
export function usePeopleRail(deps: PeopleRailDeps): ComputedRef<PeopleRailGroups> {
  return computed(() =>
    buildPeopleRail({
      selfJid: deps.selfJid.value,
      roomPresence: deps.roomPresence.value,
      authorJidByNick: deps.authorJidByNick.value,
      avatarUrlByAuthor: deps.avatarUrlByAuthor.value,
      members: deps.members.value,
      activeRoomJid: deps.activeRoomJid.value,
      callParticipants: deps.callParticipants.value,
      callParticipantOwners: deps.callParticipantOwners.value,
      ownCallRoomJid: deps.ownCallRoomJid.value,
      speakingJids: deps.speakingJids.value,
      dmCallActivities: deps.dmCallActivities.value,
      contacts: deps.contacts.value,
      conversations: deps.conversations.value,
      peerInCall: deps.peerInCall,
    }),
  );
}
