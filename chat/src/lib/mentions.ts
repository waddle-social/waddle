import type { MemberSummary } from "@/lib/chat-types";

const BROADCAST_MENTION_SET = new Set(["everyone", "here"]);

/** Fixed broadcast-mention identifiers, ordered for display. */
const BROADCAST_MENTIONS = ["everyone", "here"] as const;

function canonicalMentionIdentifier(value: string): string {
  return value.trim().replace(/^xmpp:/i, "").replace(/^@+/, "").toLowerCase();
}

export function isBroadcastMention(value: string): boolean {
  return BROADCAST_MENTION_SET.has(canonicalMentionIdentifier(value));
}

export function mentionMatchesUsername(mention: string, username?: string | null): boolean {
  if (!username) return false;
  return canonicalMentionIdentifier(mention).split("@")[0] === canonicalMentionIdentifier(username);
}

/**
 * Returns a deduplicated, display-ordered name list for mention autocomplete.
 * Broadcast mentions (`everyone`, `here`) are always listed first; member
 * names that collide with broadcast identifiers are filtered out.
 */
export function mentionAutocompleteNames(memberNames: readonly string[]): string[] {
  const filtered = memberNames.filter(
    (name) => !BROADCAST_MENTION_SET.has(name.toLowerCase()),
  );
  return [...BROADCAST_MENTIONS, ...filtered];
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
  /** Canonical nick → bare JID for all non-offline occupants whose JID is known. */
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
    jidByLowerNick[nick.toLowerCase()] = jid;
  }

  // Case-insensitive username → bare JID from the affiliation member list
  const jidByMemberUsername: Record<string, string> = {};
  for (const member of members) {
    jidByMemberUsername[member.username.toLowerCase()] = member.jid;
  }

  const existingNicks = new Set(members.map((m) => m.username.toLowerCase()));
  const anonymousNicks: string[] = [];

  for (const [nick, status] of Object.entries(roomPresence)) {
    if (status === "offline") continue;

    const lowerNick = nick.toLowerCase();
    const jidFromPresence = jidByLowerNick[lowerNick];

    if (jidFromPresence) {
      authorJidByNick[nick] = jidFromPresence;
      if (!existingNicks.has(lowerNick)) {
        mergedMembers.push({
          jid: jidFromPresence,
          username: nick,
          avatar_url: null,
          role: "member",
          joined_at: "",
        });
        existingNicks.add(lowerNick);
      }
    } else {
      const jidFromMembers = jidByMemberUsername[lowerNick];
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
