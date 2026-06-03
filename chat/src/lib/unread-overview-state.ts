import { ref, watch, type Ref } from "vue";
import type { BrowserXmppClient, LiveRoomMessage } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { TimelineMessage } from "@/lib/chat-ui";
import { type InboxState, threadsForRoom } from "@/services/inbox";
import { isFeedTimelineMessage, mapLiveRoomMessageToTimeline } from "@/channels/timeline";
import { resolveChannelIdForRoomJid } from "@/lib/threads-channel-resolve";
import { threadDisplayTitle } from "@/lib/threads-view-filters";

// How many rooms we fetch in parallel. Each unread room costs one channel
// MAM page plus one page per unread thread; a flat `Promise.all` over every
// unread room would blast dozens of IQs at once, so we cap the fan-out.
const ROOM_FETCH_CONCURRENCY = 4;
// Headroom above the unread count so the channel page still has enough
// feed-visible (non-threaded) messages left after filtering, capped at the
// MAM page ceiling.
const FETCH_HEADROOM = 20;
const MAX_PAGE = 100;

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested without a live client)
// ---------------------------------------------------------------------------

interface UnreadThreadCandidate {
  threadId: string;
  unread: number;
  title: string;
  lastUpdated: number;
}

interface UnreadRoomCandidate {
  roomJid: string;
  channelUnread: number;
  threads: UnreadThreadCandidate[];
  lastUpdated: number;
}

/**
 * Derive the set of MUC rooms that carry any unread — either channel-level
 * unread or unread threads — from the live inbox state. DMs (`kind: "direct"`)
 * are excluded. Sorted most-recently-active first.
 */
export function selectUnreadRoomCandidates(state: InboxState): UnreadRoomCandidate[] {
  const roomJids = new Set<string>();
  for (const entry of state.channels.values()) {
    if (entry.kind === "muc" && entry.unread > 0) roomJids.add(entry.partner);
  }
  for (const entry of state.threads.values()) {
    if (entry.kind === "muc" && entry.unread > 0) roomJids.add(entry.partner);
  }

  const candidates: UnreadRoomCandidate[] = [];
  for (const roomJid of roomJids) {
    const channelEntry = state.channels.get(roomJid);
    const channelUnread =
      channelEntry && channelEntry.kind === "muc" ? channelEntry.unread : 0;
    const threads: UnreadThreadCandidate[] = threadsForRoom(state, roomJid)
      .filter((entry) => entry.kind === "muc" && entry.unread > 0 && !!entry.thread)
      .map((entry) => ({
        threadId: entry.thread!,
        unread: entry.unread,
        title: threadDisplayTitle({
          ...(entry.threadTitle ? { thread_title: entry.threadTitle } : {}),
          ...(entry.preview ? { preview: entry.preview } : {}),
        }),
        lastUpdated: entry.lastUpdated,
      }));
    if (channelUnread === 0 && threads.length === 0) continue;
    const lastUpdated = Math.max(
      channelEntry?.lastUpdated ?? 0,
      ...threads.map((thread) => thread.lastUpdated),
      0,
    );
    candidates.push({ roomJid, channelUnread, threads, lastUpdated });
  }

  candidates.sort((left, right) => right.lastUpdated - left.lastUpdated);
  return candidates;
}

/**
 * The last `unread` feed-visible (non-threaded) messages of a channel page —
 * the same boundary the channel timeline uses for its "new messages" divider.
 */
export function lastFeedVisibleUnread(
  messages: readonly TimelineMessage[],
  unread: number,
): TimelineMessage[] {
  if (unread <= 0) return [];
  const feed = messages.filter(isFeedTimelineMessage);
  return feed.slice(Math.max(0, feed.length - unread));
}

/** The last `unread` messages of a thread page. */
export function lastThreadUnread(
  messages: readonly TimelineMessage[],
  unread: number,
): TimelineMessage[] {
  if (unread <= 0) return [];
  return messages.slice(Math.max(0, messages.length - unread));
}

/** Resolve the topology channel that hosts a room JID, or null if unknown. */
export function findChannelForRoomJid(
  roomJid: string,
  channels: readonly ChannelSummary[],
): ChannelSummary | null {
  const matched = channels.find((channel) => channel.jid && barePeerJid(channel.jid) === roomJid);
  if (matched) return matched;
  const slug = resolveChannelIdForRoomJid(roomJid, channels);
  return slug ? channels.find((channel) => channel.id === slug) ?? null : null;
}

/** Run `worker` over `items` with at most `limit` in flight at a time. */
async function runWithConcurrency<T, R>(
  items: readonly T[],
  limit: number,
  worker: (item: T) => Promise<R>,
): Promise<R[]> {
  const results: R[] = new Array(items.length);
  let cursor = 0;
  async function pump(): Promise<void> {
    while (cursor < items.length) {
      const index = cursor++;
      results[index] = await worker(items[index]!);
    }
  }
  const runners = Array.from({ length: Math.min(limit, items.length) }, () => pump());
  await Promise.all(runners);
  return results;
}

// ---------------------------------------------------------------------------
// Composable
// ---------------------------------------------------------------------------

interface UnreadThreadGroup {
  threadId: string;
  title: string;
  unreadCount: number;
  messages: TimelineMessage[];
}

interface UnreadChannelGroup {
  channelId: string;
  channelName: string;
  roomJid: string;
  channelUnreadCount: number;
  channelMessages: TimelineMessage[];
  threads: UnreadThreadGroup[];
  lastUpdated: number;
}

interface UnreadOverviewClient {
  queryMamPage: BrowserXmppClient["queryMamPage"];
  queryMamThreadPage: BrowserXmppClient["queryMamThreadPage"];
}

export function useUnreadOverview(deps: {
  xmppClient: Readonly<Ref<UnreadOverviewClient | null>>;
  session: Readonly<Ref<WaddleSession | null>>;
  channels: Readonly<Ref<readonly ChannelSummary[]>>;
  inboxState: Readonly<Ref<InboxState>>;
}) {
  const groups = ref<UnreadChannelGroup[]>([]);
  const isLoading = ref(false);
  const error = ref<string | null>(null);
  let requestSerial = 0;

  function fetchSize(unread: number): number {
    return Math.min(MAX_PAGE, unread + FETCH_HEADROOM);
  }

  async function buildGroup(
    client: UnreadOverviewClient,
    session: WaddleSession,
    candidate: UnreadRoomCandidate,
  ): Promise<UnreadChannelGroup | null> {
    const channel = findChannelForRoomJid(candidate.roomJid, deps.channels.value);
    if (!channel) return null;
    const spaceId = channel.spaceId ?? "";
    const channelId = channel.id;

    const channelMessages = candidate.channelUnread > 0
      ? lastFeedVisibleUnread(
        (await client.queryMamPage(spaceId, channelId, fetchSize(candidate.channelUnread), {
          type: "latest",
        })).messages.map((msg: LiveRoomMessage) => mapLiveRoomMessageToTimeline(session, msg)),
        candidate.channelUnread,
      )
      : [];

    const threads = await Promise.all(
      candidate.threads.map(async (thread): Promise<UnreadThreadGroup> => {
        const page = await client.queryMamThreadPage(
          spaceId,
          channelId,
          thread.threadId,
          fetchSize(thread.unread),
          { type: "latest" },
        );
        return {
          threadId: thread.threadId,
          title: thread.title,
          unreadCount: thread.unread,
          messages: lastThreadUnread(
            page.messages.map((msg: LiveRoomMessage) => mapLiveRoomMessageToTimeline(session, msg)),
            thread.unread,
          ),
        };
      }),
    );

    if (channelMessages.length === 0 && threads.every((thread) => thread.messages.length === 0)) {
      return null;
    }

    return {
      channelId,
      channelName: channel.name || channelId,
      roomJid: candidate.roomJid,
      channelUnreadCount: candidate.channelUnread,
      channelMessages,
      threads,
      lastUpdated: candidate.lastUpdated,
    };
  }

  async function refresh(): Promise<void> {
    const client = deps.xmppClient.value;
    const session = deps.session.value;
    const serial = ++requestSerial;
    if (!client || !session) {
      groups.value = [];
      isLoading.value = false;
      return;
    }

    const candidates = selectUnreadRoomCandidates(deps.inboxState.value);
    isLoading.value = true;
    error.value = null;
    try {
      const built = await runWithConcurrency(candidates, ROOM_FETCH_CONCURRENCY, (candidate) =>
        buildGroup(client, session, candidate),
      );
      if (serial !== requestSerial) return;
      groups.value = built.filter((group): group is UnreadChannelGroup => group !== null);
    } catch (err) {
      if (serial !== requestSerial) return;
      error.value = err instanceof Error ? err.message : String(err);
    } finally {
      if (serial === requestSerial) isLoading.value = false;
    }
  }

  watch(
    [deps.inboxState, deps.xmppClient, deps.session],
    () => {
      void refresh();
    },
    { immediate: true },
  );

  return { groups, isLoading, error, refresh };
}
