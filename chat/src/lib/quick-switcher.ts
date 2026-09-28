import { groupChannelsBySpace } from "@/lib/channel-grouping";
import type { ChannelSummary, GroupDmSummary, SpaceSummary } from "@/lib/chat-types";
import { bareJidKey } from "@/lib/xmpp/jid";
import type { DmConversation, RosterContact } from "@/lib/xmpp/types";

/**
 * Cmd+K quick switcher entries and ranking. Mirrors the Apple client's
 * `QuickSwitcherRanking` / `QuickSwitcherCursor` so both clients list and
 * order the same places the same way.
 */

export type QuickSwitcherPage =
  | "home"
  | "rooms"
  | "threads"
  | "unread"
  | "members"
  | "feed"
  | "events"
  | "settings";

type QuickSwitcherTarget =
  | { kind: "channel"; channelId: string; roomJid?: string }
  | { kind: "groupDm"; roomJid: string }
  | { kind: "dm"; peerJid: string }
  | { kind: "page"; page: QuickSwitcherPage };

export interface QuickSwitcherEntry {
  /** Unique across kinds. */
  id: string;
  title: string;
  /** Waddle name for rooms (only with more than one waddle), "Group chat", or the address. */
  subtitle?: string;
  unread: number;
  mentionsMe: boolean;
  target: QuickSwitcherTarget;
  avatarUrl?: string | null;
  forum?: boolean;
}

interface QuickSwitcherSources {
  spaces: SpaceSummary[];
  channels: ChannelSummary[];
  /** Unread and mention counts by channel id. */
  channelUnread: Readonly<Record<string, { unread: number; mentions: number }>>;
  groupDms: readonly GroupDmSummary[];
  /** Newest first, as the DM store keeps them. */
  conversations: readonly DmConversation[];
  contacts: readonly RosterContact[];
}

const PAGE_ENTRIES: readonly QuickSwitcherEntry[] = ([
  ["home", "Home"],
  ["rooms", "Rooms"],
  ["threads", "Discussions"],
  ["unread", "Unread"],
  ["members", "Members"],
  ["feed", "Feed"],
  ["events", "Events"],
  ["settings", "Settings"],
] as const).map(([page, title]): QuickSwitcherEntry => ({
  id: `page:${page}`,
  title,
  unread: 0,
  mentionsMe: false,
  target: { kind: "page", page },
}));

/**
 * Rooms by waddle, then group chats, account DMs (newest first), contacts
 * without a DM, and pages. MUC private chats are left out: a room nick is not
 * a person we know by JID (#1256), and a nick like `alice@example.com` would
 * otherwise outrank the real account.
 */
export function buildQuickSwitcherEntries(sources: QuickSwitcherSources): QuickSwitcherEntry[] {
  const accountConversations = sources.conversations.filter((conversation) => !conversation.mucPm);
  return [
    ...channelEntries(sources),
    ...sources.groupDms.map(groupDmEntry),
    ...accountConversations.map(conversationEntry),
    ...contactEntries(sources.contacts, accountConversations),
    ...PAGE_ENTRIES,
  ];
}

function channelEntries({ spaces, channels, channelUnread }: QuickSwitcherSources): QuickSwitcherEntry[] {
  const showsSpace = spaces.length > 1;
  const rooms = channels.filter((channel) => !channel.isGroupDm);
  return groupChannelsBySpace(spaces, rooms).flatMap((group) =>
    group.channels.map((channel): QuickSwitcherEntry => {
      const counts = channelUnread[channel.id];
      return {
        id: `channel:${channel.id}`,
        title: channel.name,
        subtitle: showsSpace ? group.space?.name : undefined,
        unread: counts?.unread ?? 0,
        mentionsMe: (counts?.mentions ?? 0) > 0,
        target: channel.jid
          ? { kind: "channel", channelId: channel.id, roomJid: channel.jid }
          : { kind: "channel", channelId: channel.id },
        forum: channel.channel_type === "forum",
      };
    }),
  );
}

function groupDmEntry(group: GroupDmSummary): QuickSwitcherEntry {
  return {
    id: `groupDm:${group.roomJid}`,
    title: group.name,
    subtitle: "Group chat",
    unread: group.unreadCount ?? 0,
    mentionsMe: (group.mentionCount ?? 0) > 0,
    target: { kind: "groupDm", roomJid: group.roomJid },
  };
}

function conversationEntry(conversation: DmConversation): QuickSwitcherEntry {
  return {
    id: `dm:${conversation.peerJid}`,
    title: conversation.peerUsername,
    subtitle: conversation.peerJid,
    unread: conversation.unreadCount,
    mentionsMe: false,
    target: { kind: "dm", peerJid: conversation.peerJid },
    avatarUrl: conversation.peerAvatarUrl ?? null,
  };
}

/** Roster contacts with no account DM yet. */
function contactEntries(
  contacts: readonly RosterContact[],
  conversations: readonly DmConversation[],
): QuickSwitcherEntry[] {
  const withConversation = new Set(conversations.map((conversation) => bareJidKey(conversation.peerJid)));
  return contacts
    .filter((contact) => !withConversation.has(bareJidKey(contact.jid)))
    .map((contact): QuickSwitcherEntry => ({
      id: `contact:${bareJidKey(contact.jid)}`,
      title: contact.name || contact.username || contact.jid,
      subtitle: contact.jid,
      unread: 0,
      mentionsMe: false,
      target: { kind: "dm", peerJid: contact.jid },
    }))
    .sort((a, b) => a.title.localeCompare(b.title, undefined, { sensitivity: "base" }));
}

/**
 * With no query: mentions, then unread, then the rest in list order.
 * With a query: title prefix, word prefix, title substring, then subtitle
 * substring; ties keep the no-query order.
 */
export function rankQuickSwitcherEntries(
  entries: readonly QuickSwitcherEntry[],
  query: string,
  limit = 50,
): QuickSwitcherEntry[] {
  const needle = normalizedQuery(query);
  return entries
    .flatMap((entry, index) => {
      const score = matchScore(entry, needle);
      return score === null ? [] : [{ entry, index, score }];
    })
    .sort((a, b) => a.score - b.score || attentionRank(a.entry) - attentionRank(b.entry) || a.index - b.index)
    .slice(0, Math.max(0, limit))
    .map(({ entry }) => entry);
}

/** Trimmed, without leading `#`/`@` (so `#general` finds `general`), case- and accent-folded. */
function normalizedQuery(query: string): string {
  return folded(query.trim().replace(/^[#@]+/, "").trim());
}

function folded(text: string): string {
  return text.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
}

/** Lower is better; null drops the entry. */
function matchScore(entry: QuickSwitcherEntry, needle: string): number | null {
  if (!needle) return 0;
  const title = folded(entry.title);
  if (title.startsWith(needle)) return 0;
  if (title.split(/[^\p{L}\p{N}]+/u).some((word) => word.startsWith(needle))) return 1;
  if (title.includes(needle)) return 2;
  if (entry.subtitle && folded(entry.subtitle).includes(needle)) return 3;
  return null;
}

function attentionRank(entry: QuickSwitcherEntry): number {
  if (entry.mentionsMe) return 0;
  if (entry.unread > 0) return 1;
  return 2;
}

/** Keeps the highlight when it is still listed, else the first row. */
export function resolveQuickSwitcherHighlight(current: string | null, ids: readonly string[]): string | null {
  if (current !== null && ids.includes(current)) return current;
  return ids[0] ?? null;
}

/** Moves by `offset` rows, wrapping at either end. */
export function moveQuickSwitcherHighlight(
  current: string | null,
  offset: number,
  ids: readonly string[],
): string | null {
  if (ids.length === 0) return null;
  const index = current === null ? -1 : ids.indexOf(current);
  if (index < 0) return (offset < 0 ? ids[ids.length - 1] : ids[0]) ?? null;
  const count = ids.length;
  return ids[((index + offset) % count + count) % count] ?? null;
}
