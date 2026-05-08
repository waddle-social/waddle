import type { MemberSummary } from "@/lib/chat-types";
import type { TimelineMessage } from "@/lib/chat-ui";

const BROADCAST_MENTION_SET = new Set(["everyone", "here"]);
const MENTIONABLE_MEMBER_ROLES = new Set<MemberSummary["role"]>(["owner", "admin", "member"]);

/** Fixed broadcast-mention identifiers, ordered for display. */
const BROADCAST_MENTIONS = ["everyone", "here"] as const;

export interface MentionCandidate {
  username: string;
  jid: string | null;
  avatar_url: string | null;
  kind: "broadcast" | "member";
}

interface AvatarLookupCandidate {
  nick: string;
  jid: string;
  avatar_url: string | null;
}

function canonicalMentionIdentifier(value: string): string {
  return value.trim().replace(/^xmpp:/i, "").replace(/^@+/, "").split(/[?#]/)[0]?.toLowerCase() ?? "";
}

function bareJid(value?: string | null): string | null {
  if (!value || !value.includes("@")) return null;
  return value.split("/")[0] || null;
}

function isMucOccupantJid(jid: string): boolean {
  const bare = bareJid(jid) ?? jid;
  return jid.includes("/") && bare.split("@")[1]?.startsWith("muc.") === true;
}

function inferLocalUserJid(nick: string, selfDomain: string): string | null {
  const canonical = canonicalMentionIdentifier(nick);
  if (!canonical || canonical.includes("@") || !selfDomain) return null;
  return `${canonical}@${selfDomain}`;
}

export function mentionMatchesUsername(mention: string, username?: string | null): boolean {
  if (!username) return false;
  return canonicalMentionIdentifier(mention).split("@")[0] === canonicalMentionIdentifier(username);
}

export function mentionMatchesBareJid(mention: string, jid?: string | null): boolean {
  const mentionedBareJid = bareJid(canonicalMentionIdentifier(mention));
  const targetBareJid = bareJid(canonicalMentionIdentifier(jid ?? ""));
  return !!mentionedBareJid && !!targetBareJid && mentionedBareJid === targetBareJid;
}

export function messageMentionsBareJid(
  message: Pick<TimelineMessage, "broadcastMention" | "mentions">,
  jid?: string | null,
): boolean {
  if (message.broadcastMention) return true;
  if (!jid || !message.mentions) return false;
  return message.mentions.some((mention) => mentionMatchesBareJid(mention, jid));
}

/**
 * Returns a deduplicated, display-ordered name list for mention autocomplete.
 * Broadcast mentions (`everyone`, `here`) are always listed first; member
 * names that collide with broadcast identifiers are filtered out.
 */
export function mentionAutocompleteNames(memberNames: readonly string[]): string[] {
  const filtered = memberNames.filter(
    (name) => !BROADCAST_MENTION_SET.has(canonicalMentionIdentifier(name)),
  );
  return [...BROADCAST_MENTIONS, ...filtered];
}

export function mentionAutocompleteCandidates(
  members: readonly MemberSummary[],
): MentionCandidate[] {
  const candidates: MentionCandidate[] = BROADCAST_MENTIONS.map((username) => ({
    username,
    jid: null,
    avatar_url: null,
    kind: "broadcast",
  }));
  const seen = new Set<string>(BROADCAST_MENTIONS);

  for (const member of members) {
    const canonical = canonicalMentionIdentifier(member.username);
    if (!canonical || seen.has(canonical) || !MENTIONABLE_MEMBER_ROLES.has(member.role)) {
      continue;
    }
    candidates.push({
      username: member.username,
      jid: member.jid,
      avatar_url: member.avatar_url,
      kind: "member",
    });
    seen.add(canonical);
  }

  return candidates;
}

export function avatarLookupCandidates({
  members,
  messages,
  authorJidByNick,
  selfDomain,
}: {
  members: readonly MemberSummary[];
  messages: readonly Pick<TimelineMessage, "author" | "authorJid" | "authorRealJid">[];
  authorJidByNick: Readonly<Record<string, string>>;
  selfDomain: string;
}): AvatarLookupCandidate[] {
  const candidates: AvatarLookupCandidate[] = [];
  const seenJids = new Set<string>();
  const memberAvatarByJid = new Map<string, string | null>();
  const mappedJidByNick = new Map<string, string>();

  for (const member of members) {
    const jid = bareJid(member.jid);
    if (!jid) continue;
    memberAvatarByJid.set(jid, member.avatar_url);
    mappedJidByNick.set(canonicalMentionIdentifier(member.username), jid);
  }
  for (const [nick, jid] of Object.entries(authorJidByNick)) {
    const bare = bareJid(jid);
    if (bare) mappedJidByNick.set(canonicalMentionIdentifier(nick), bare);
  }

  function add(nick: string, jid: string, avatar_url: string | null): void {
    const bare = bareJid(jid);
    if (!bare || seenJids.has(bare)) return;
    seenJids.add(bare);
    candidates.push({ nick, jid: bare, avatar_url });
  }

  for (const member of members) {
    const jid = bareJid(member.jid);
    if (jid) add(member.username, jid, member.avatar_url);
  }

  const bestMessageCandidateByNick = new Map<string, { nick: string; jid: string; rank: number }>();
  for (const message of messages) {
    const nick = message.author;
    const mappedJid = mappedJidByNick.get(canonicalMentionIdentifier(nick));
    const realJid = bareJid(message.authorRealJid);
    const messageJid = message.authorJid && !isMucOccupantJid(message.authorJid)
      ? bareJid(message.authorJid)
      : null;
    const resolved = realJid
      ? { jid: realJid, rank: 0 }
      : mappedJid
        ? { jid: mappedJid, rank: 1 }
        : messageJid
          ? { jid: messageJid, rank: 2 }
          : (() => {
              const inferred = inferLocalUserJid(nick, selfDomain);
              return inferred ? { jid: inferred, rank: 3 } : null;
            })();
    if (!resolved) continue;
    const key = canonicalMentionIdentifier(nick);
    const previous = bestMessageCandidateByNick.get(key);
    if (!previous || resolved.rank < previous.rank) {
      bestMessageCandidateByNick.set(key, { nick, ...resolved });
    }
  }

  for (const { nick, jid } of bestMessageCandidateByNick.values()) {
    if (!jid) continue;
    add(nick, jid, memberAvatarByJid.get(jid) ?? null);
  }

  return candidates;
}

export function resolveMentionUri(
  mention: string,
  mentionJidsByNick?: Readonly<Record<string, string>>,
): string {
  const canonicalMention = canonicalMentionIdentifier(mention);
  const mappedBareJid = mentionJidsByNick
    ? Object.entries(mentionJidsByNick).find(([nick]) =>
        canonicalMentionIdentifier(nick) === canonicalMention
      )?.[1]
    : undefined;

  return `xmpp:${canonicalMentionIdentifier(mappedBareJid ?? canonicalMention)}`;
}

interface MergeMentionMembersParams {
  /** Authoritative member list from the MUC affiliation query. */
  members: MemberSummary[];
  /** Live presence map: nick → presence status string. */
  roomPresence: Readonly<Record<string, string>>;
  /**
   * Nick-to-bare-JID map populated from in-room presence stanzas that carry
   * the occupant's real JID (non-anonymous rooms only).
   */
  memberJidsByNick: Readonly<Record<string, string>>;
}

interface MergeMentionMembersResult {
  /** Merged list: original members plus any presence occupants with resolvable bare JIDs. */
  members: MemberSummary[];
  /** Canonical nick → bare JID for all registered members and non-offline occupants whose JID is known. */
  authorJidByNick: Record<string, string>;
  /** Human-readable diagnostics for incomplete JID resolution (for telemetry/dev). */
  diagnostics: string[];
}

/**
 * Merges live MUC presence occupants into the authoritative member list and
 * builds a nick→bareJID index for mention resolution.
 *
 * - Occupants with a JID in `memberJidsByNick` are added to members if absent.
 * - Occupants found only in the affiliation `members` list (not in
 *   `memberJidsByNick`) trigger a "missing bare JID" diagnostic.
 * - Occupants with no JID from either source trigger an "anonymous room"
 *   diagnostic and are omitted from the returned member list.
 */
export function mergeMentionMembers({
  members,
  roomPresence,
  memberJidsByNick,
}: MergeMentionMembersParams): MergeMentionMembersResult {
  const mergedMembers = [...members];
  const authorJidByNick: Record<string, string> = {};
  const diagnostics: string[] = [];

  // Case-insensitive nick → bare JID from presence stanzas
  const jidByLowerNick: Record<string, string> = {};
  for (const [nick, jid] of Object.entries(memberJidsByNick)) {
    jidByLowerNick[canonicalMentionIdentifier(nick)] = jid;
  }

  // Case-insensitive username → bare JID from the affiliation member list
  const jidByMemberUsername: Record<string, string> = {};
  for (const member of members) {
    const canonical = canonicalMentionIdentifier(member.username);
    jidByMemberUsername[canonical] = member.jid;
    if (MENTIONABLE_MEMBER_ROLES.has(member.role)) {
      authorJidByNick[member.username] = member.jid;
    }
  }

  const existingNicks = new Set(members.map((m) => canonicalMentionIdentifier(m.username)));
  const anonymousNicks: string[] = [];

  for (const [nick, status] of Object.entries(roomPresence)) {
    if (status === "offline") continue;

    const canonicalNick = canonicalMentionIdentifier(nick);
    const jidFromPresence = jidByLowerNick[canonicalNick];

    if (jidFromPresence) {
      authorJidByNick[nick] = jidFromPresence;
      if (!existingNicks.has(canonicalNick)) {
        mergedMembers.push({
          jid: jidFromPresence,
          username: nick,
          avatar_url: null,
          role: "member",
          joined_at: "",
        });
        existingNicks.add(canonicalNick);
      }
    } else {
      const jidFromMembers = jidByMemberUsername[canonicalNick];
      if (jidFromMembers) {
        // JID is resolvable from the affiliation list but the server didn't
        // include it in presence stanzas — partial anonymity or ordering issue.
        authorJidByNick[nick] = jidFromMembers;
        diagnostics.push(`Presence invariant violated: missing bare occupant JIDs for ${nick}.`);
      } else {
        anonymousNicks.push(nick);
      }
    }
  }

  if (anonymousNicks.length > 0) {
    diagnostics.push(
      `Presence invariant violated: room appears anonymous or omitted bare occupant JIDs for ${anonymousNicks.join(", ")}.`,
    );
  }

  return { members: mergedMembers, authorJidByNick, diagnostics };
}
