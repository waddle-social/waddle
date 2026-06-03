import { onScopeDispose, ref, watch, type Ref } from "vue";
import type { BrowserXmppClient, LiveRoomMessage } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
import { isForumChannel } from "@/lib/channel-types";
import type { WaddleSession } from "@/lib/server-auth";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { WasmArchivedMessage } from "@/lib/xmpp/wasm-types";
import { roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import { trustedLinkPreviewMediaOrigin } from "@/lib/xmpp/link-preview";
import { findMessageById } from "@/lib/message-ids";
import { type InboxState, threadsForRoom } from "@/services/inbox";
import { isFeedTimelineMessage } from "@/channels/timeline";
import {
  buildChannelTimelineFromMamResults,
  isMucServiceModeration,
  isSameMucCorrectionSender,
  isSameMucRetractionSender,
  isValidMucModerationTarget,
  isValidMucRetractionTarget,
  mucCorrectionSender,
} from "@/channels/message-timeline-state";
import { resolveChannelIdForRoomJid } from "@/lib/threads-channel-resolve";
import { threadDisplayTitle } from "@/lib/threads-view-filters";

// How many rooms we fetch in parallel. Each unread room costs one channel
// MAM page plus one page per unread thread; a flat `Promise.all` over every
// unread room would blast dozens of IQs at once, so we cap the fan-out.
const ROOM_FETCH_CONCURRENCY = 4;
// Thread pages are MAM IQs too. Cap them per room so one busy room cannot
// fan out an unbounded burst while room-level concurrency is already maxed.
const THREAD_FETCH_CONCURRENCY = ROOM_FETCH_CONCURRENCY;
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

function findTimelineMessageByProtocolId(
  timeline: readonly TimelineMessage[],
  id: string | undefined,
): TimelineMessage | undefined {
  const normalized = id?.trim();
  if (!normalized) return undefined;
  return findMessageById(timeline, normalized)
    ?? timeline.find((message) =>
      message.replyableId === normalized
      || message.reactionTargetId === normalized
      || message.stanzaId === normalized
    );
}

function timelineMessageForMamHit(
  timeline: readonly TimelineMessage[],
  msg: LiveRoomMessage,
): TimelineMessage | undefined {
  if (msg._reactionTarget) {
    if (!msg._reactionEmojis) return undefined;
    return timeline.find((message) => message.reactionTargetId === msg._reactionTarget);
  }
  if (msg.retractsId) {
    const target = findMessageById(timeline, msg.retractsId);
    if (!target) return undefined;
    if (msg.moderationTargetId) {
      return isMucServiceModeration(msg) && isValidMucModerationTarget(target, msg.retractsId)
        ? target
        : undefined;
    }
    return isValidMucRetractionTarget(target, msg.retractsId)
      && isSameMucRetractionSender(target, mucCorrectionSender(msg))
      ? target
      : undefined;
  }
  if (msg.replacesId) {
    const target = findMessageById(timeline, msg.replacesId);
    return target && isSameMucCorrectionSender(target, mucCorrectionSender(msg))
      ? target
      : undefined;
  }
  return findTimelineMessageByProtocolId(timeline, msg.id);
}

function lastUnreadFromMamHits(
  timeline: readonly TimelineMessage[],
  mamResults: readonly LiveRoomMessage[],
  unread: number,
  isVisible: (message: TimelineMessage) => boolean,
): TimelineMessage[] {
  if (unread <= 0) return [];
  const selected: TimelineMessage[] = [];
  const seen = new Set<string>();
  for (let index = mamResults.length - 1; index >= 0 && selected.length < unread; index -= 1) {
    const message = timelineMessageForMamHit(timeline, mamResults[index]!);
    if (!message || !isVisible(message) || seen.has(message.id)) continue;
    seen.add(message.id);
    selected.push(message);
  }
  return selected.reverse();
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

function missingMamUpdateTargets(
  mamResults: readonly LiveRoomMessage[],
  timeline: readonly TimelineMessage[],
): string[] {
  const targets: string[] = [];
  const seen = new Set<string>();
  const add = (targetId: string | undefined, isPresent: (id: string) => boolean) => {
    const normalized = targetId?.trim();
    if (!normalized || seen.has(normalized) || isPresent(normalized)) return;
    seen.add(normalized);
    targets.push(normalized);
  };

  for (const msg of mamResults) {
    add(msg.retractsId, (id) => !!findMessageById(timeline, id));
    add(msg._reactionTarget, (id) => timeline.some((message) => message.reactionTargetId === id));
    if (targets.length >= MAX_PAGE) break;
  }
  return targets;
}

function matchesRequestedRoomStanzaId(
  archived: WasmArchivedMessage,
  roomJid: string,
  requested: ReadonlySet<string>,
): boolean {
  const roomStamped = archived.stanza_ids?.find((stanzaId) => stanzaId.by === roomJid)?.id;
  if (roomStamped && requested.has(roomStamped)) return true;
  return !!archived.stanza_id && archived.stanza_id_by === roomJid && requested.has(archived.stanza_id);
}

function liveRoomMessagesFromArchived(
  session: WaddleSession,
  roomJid: string,
  requestedStanzaIds: readonly string[],
  archived: readonly WasmArchivedMessage[],
): LiveRoomMessage[] {
  const trustedMediaOrigin = trustedLinkPreviewMediaOrigin(session);
  const requested = new Set(requestedStanzaIds);
  return archived.flatMap((message) => {
    if (!matchesRequestedRoomStanzaId(message, roomJid, requested)) return [];
    const live = roomMessageFromArchived(message, { trustedMediaOrigin });
    return live ? [live] : [];
  });
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
  fetchRoomMessagesByStanzaIds?: BrowserXmppClient["fetchRoomMessagesByStanzaIds"];
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
  let disposed = false;

  function fetchSize(unread: number): number {
    return Math.min(MAX_PAGE, unread + FETCH_HEADROOM);
  }

  async function buildGroup(
    client: UnreadOverviewClient,
    session: WaddleSession,
    candidate: UnreadRoomCandidate,
    serial: number,
  ): Promise<UnreadChannelGroup | null> {
    const channel = findChannelForRoomJid(candidate.roomJid, deps.channels.value);
    if (!channel) return null;
    if (serial !== requestSerial) return null;
    const spaceId = channel.spaceId ?? "";
    const channelId = channel.id;
    const timelineFromMam = async (mamResults: LiveRoomMessage[]): Promise<TimelineMessage[]> => {
      const timeline = buildChannelTimelineFromMamResults({
        session,
        channelIsForum: isForumChannel(channel),
        mamResults,
      });
      const missingTargets = missingMamUpdateTargets(mamResults, timeline);
      if (
        missingTargets.length === 0
        || serial !== requestSerial
        || !client.fetchRoomMessagesByStanzaIds
      ) {
        return timeline;
      }

      try {
        const archivedTargets = await client.fetchRoomMessagesByStanzaIds(
          spaceId,
          channelId,
          missingTargets,
        );
        if (serial !== requestSerial) return timeline;
        const targetMessages = liveRoomMessagesFromArchived(
          session,
          candidate.roomJid,
          missingTargets,
          archivedTargets,
        );
        if (targetMessages.length === 0) return timeline;
        return buildChannelTimelineFromMamResults({
          session,
          channelIsForum: isForumChannel(channel),
          mamResults: [...targetMessages, ...mamResults],
        });
      } catch {
        return timeline;
      }
    };

    let channelMessages: TimelineMessage[] = [];
    if (candidate.channelUnread > 0) {
      const page = await client.queryMamPage(spaceId, channelId, fetchSize(candidate.channelUnread), {
        type: "latest",
      });
      channelMessages = lastUnreadFromMamHits(
        await timelineFromMam(page.messages),
        page.messages,
        candidate.channelUnread,
        isFeedTimelineMessage,
      );
    }

    if (serial !== requestSerial) return null;

    const threads = await runWithConcurrency(
      candidate.threads,
      THREAD_FETCH_CONCURRENCY,
      async (thread): Promise<UnreadThreadGroup> => {
        if (serial !== requestSerial) {
          return {
            threadId: thread.threadId,
            title: thread.title,
            unreadCount: thread.unread,
            messages: [],
          };
        }
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
          messages: lastUnreadFromMamHits(
            await timelineFromMam(page.messages),
            page.messages,
            thread.unread,
            () => true,
          ),
        };
      },
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
    if (disposed) return;
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
        buildGroup(client, session, candidate, serial),
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
    [deps.inboxState, deps.xmppClient, deps.session, deps.channels],
    () => {
      void refresh();
    },
    { immediate: true },
  );

  onScopeDispose(() => {
    disposed = true;
    requestSerial += 1;
    isLoading.value = false;
  });

  return { groups, isLoading, error, refresh };
}
