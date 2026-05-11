import { nextTick, ref, type Ref } from "vue";
import type { ChannelSummary } from "@/lib/chat-types";
import { isForumChannel } from "@/lib/channel-types";
import type { WaddleSession } from "@/lib/server-auth";
import type {
  BrowserXmppClient,
  LiveRoomMessage,
} from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import {
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import {
  buildChannelTimelineFromMamResults,
  type TimelineBuildOptions,
} from "@/channels/message-timeline-state";
import { isFeedTimelineMessage } from "@/channels/timeline";
import {
  advanceCursorWithBeforePage,
  advanceThreadCursor,
  classifyMamError,
  cursorFromLatestPage,
  isMamCursorNotFound,
  stripQueuedSelfMessages,
  threadCursorFromLatestPage,
} from "@/lib/xmpp/mam-paging";
import { hydratePinnedRoom, pinnedRoomsEpoch } from "@/stores/pinned-messages";

const PAGE_SIZE = 100;

type UseChannelMamPagingDeps = {
  session: Ref<WaddleSession | null>;
  xmppClient: Ref<BrowserXmppClient | null>;
  activeSpaceId: Ref<string | null>;
  activeChannelId: Ref<string | null>;
  currentChannel: Ref<ChannelSummary | null>;
  messages: Ref<TimelineMessage[]>;
  firstUnseenId: Ref<string | null>;
  timelineEl: Ref<HTMLDivElement | null>;
  scrollDirection: Ref<ScrollDirectionMode>;
  pinnedEdgeScroller: { disconnect: () => void };
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  pendingEchoClientIds: Set<string>;
  appendQueuedMessages: (timeline: TimelineMessage[], roomJid: string) => TimelineMessage[];
  roomJidForChannel: (channelId: string) => string | null;
  scrollToPinnedEdgeAndPin: () => Promise<boolean>;
  persistLastSeen: (channelId: string, messageId: string) => void;
};

export function useChannelMamPaging(deps: UseChannelMamPagingDeps) {
  const {
    session,
    xmppClient,
    activeSpaceId,
    activeChannelId,
    currentChannel,
    messages,
    firstUnseenId,
    timelineEl,
    scrollDirection,
    pinnedEdgeScroller,
    actionError,
    clearActionError,
    normalizeError,
    pendingEchoClientIds,
    appendQueuedMessages,
    roomJidForChannel,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
  } = deps;

  const isLoadingMessages = ref(false);
  const isLoadingOlderMessages = ref(false);
  const hasOlderMessages = ref(true);
  const loadingOlderThreadIds = ref<Set<string>>(new Set());
  const threadHasOlder = ref<Record<string, boolean>>({});

  let messageRequestId = 0;
  let oldestArchiveId: string | null = null;
  let initialLatestPagePinned = false;
  const oldestThreadArchiveIds = new Map<string, string>();

  function buildTimelineFromMamResults(
    mamResults: LiveRoomMessage[],
    existing: TimelineMessage[] = [],
    options: TimelineBuildOptions = {},
  ): TimelineMessage[] {
    if (!session.value) return existing;
    return buildChannelTimelineFromMamResults({
      session: session.value,
      channelIsForum: isForumChannel(currentChannel.value),
      mamResults,
      existing,
      options,
    });
  }

  const isFeedVisible = isFeedTimelineMessage;

  async function loadMessages(
    spaceId: string,
    channelId: string,
    unreadAtLoad = 0,
    metadataSeed: TimelineMessage[] = [],
  ) {
    if (!session.value) return;

    const requestId = ++messageRequestId;
    const roomJid = roomJidForChannel(channelId);
    if (!roomJid) return;
    initialLatestPagePinned = false;
    isLoadingMessages.value = true;
    isLoadingOlderMessages.value = false;
    hasOlderMessages.value = true;
    oldestArchiveId = null;
    loadingOlderThreadIds.value = new Set();
    threadHasOlder.value = {};
    oldestThreadArchiveIds.clear();
    pinnedEdgeScroller.disconnect();
    // Reset the divider anchor up-front: a previous conversation's id could
    // coincidentally match a message in the new timeline, and an aborted
    // request (requestId mismatch) would otherwise leave stale state.
    firstUnseenId.value = null;
    clearActionError();
    pendingEchoClientIds.clear();
    messages.value = appendQueuedMessages([], roomJid);

    try {
      // #414: hydrate the pin store for this room. Fire-and-forget — the
      // panel + badge tolerate an empty store and the live pin-event
      // handler will mutate it from now on. Capture the epoch at request
      // time so a late callback after logout drops itself.
      if (xmppClient.value && "fetchRoomPins" in xmppClient.value) {
        const epoch = pinnedRoomsEpoch();
        void xmppClient.value
          .fetchRoomPins(spaceId, channelId)
          .then((entries) => {
            hydratePinnedRoom(roomJid, entries, epoch);
          })
          .catch((error: unknown) => {
            console.warn("fetchRoomPins failed", error);
            hydratePinnedRoom(roomJid, [], epoch);
          });
      } else {
        hydratePinnedRoom(roomJid, []);
      }

      // XEP-0313: Load message history via MAM.
      const page = xmppClient.value && "queryMamPage" in xmppClient.value
        ? await xmppClient.value.queryMamPage(spaceId, channelId, PAGE_SIZE, { type: "latest" })
        : null;
      const mamResults = page
        ? page.messages
        : xmppClient.value
          ? await xmppClient.value.queryMam(spaceId, channelId, PAGE_SIZE)
          : [];

      if (
        requestId !== messageRequestId ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }

      const cursor = cursorFromLatestPage({
        page,
        pageSize: PAGE_SIZE,
        fallbackMessages: page ? undefined : mamResults,
      });
      oldestArchiveId = cursor.oldestArchiveId;
      hasOlderMessages.value = cursor.hasOlderMessages;
      const timelineWithQueue = appendQueuedMessages(
        buildTimelineFromMamResults(mamResults, metadataSeed, {
          seedExistingOnly: metadataSeed.length > 0,
        }),
        roomJid,
      );
      messages.value = timelineWithQueue;
      if (requestId === messageRequestId) {
        isLoadingMessages.value = false;
      }

      const feedTimeline = timelineWithQueue.filter(isFeedVisible);
      firstUnseenId.value = unreadAtLoad > 0 && feedTimeline.length >= unreadAtLoad
        ? feedTimeline[feedTimeline.length - unreadAtLoad]?.id ?? null
        : null;
      const pinned = await scrollToPinnedEdgeAndPin();
      if (
        !pinned ||
        requestId !== messageRequestId ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }
      initialLatestPagePinned = true;
      const newest = [...timelineWithQueue].reverse().find(isFeedVisible);
      if (newest) persistLastSeen(channelId, newest.id);
    } catch (e) {
      if (requestId === messageRequestId) {
        const queuedOnly = appendQueuedMessages([], roomJid);
        messages.value = queuedOnly;
        actionError.value = queuedOnly.length > 0 ? "" : normalizeError(e);
        isLoadingMessages.value = false;
      }
    }
  }

  async function loadOlderMessages() {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    const before = oldestArchiveId;
    if (
      !client ||
      !channelId ||
      !before ||
      !initialLatestPagePinned ||
      !hasOlderMessages.value ||
      isLoadingOlderMessages.value
    ) {
      return;
    }
    if (!("queryMamPage" in client)) return;
    const requestId = messageRequestId;
    const isCurrentRequest = () =>
      requestId === messageRequestId &&
      xmppClient.value === client &&
      (activeSpaceId.value ?? "") === spaceId &&
      activeChannelId.value === channelId;

    const el = timelineEl.value;
    const previousHeight = el?.scrollHeight ?? 0;
    const previousTop = el?.scrollTop ?? 0;
    isLoadingOlderMessages.value = true;
    try {
      const page = await client.queryMamPage(spaceId, channelId, PAGE_SIZE, { type: "before", before });
      if (!isCurrentRequest()) return;
      const advanced = advanceCursorWithBeforePage({
        prior: { oldestArchiveId, hasOlderMessages: hasOlderMessages.value },
        page,
      });
      oldestArchiveId = advanced.next.oldestArchiveId;
      hasOlderMessages.value = advanced.next.hasOlderMessages;
      const withoutQueued = stripQueuedSelfMessages(messages.value);
      const roomJid = roomJidForChannel(channelId);
      if (!roomJid) return;
      messages.value = appendQueuedMessages(buildTimelineFromMamResults(page.messages, withoutQueued), roomJid);
      await nextTick();
      if (el && !isTopPinnedScrollDirection(scrollDirection.value)) {
        el.scrollTop = previousTop + (el.scrollHeight - previousHeight);
      }
    } catch (e) {
      const classified = classifyMamError(e);
      if (isMamCursorNotFound(classified)) {
        // XEP-0313 §4.3.4: the server reports the `<before/>` UID is gone
        // from the archive. The cursor moved past us (e.g. server-side
        // compaction). Drop the stale cursor and re-fetch the tail page —
        // safer than reporting "no more history" to the user.
        if (isCurrentRequest()) {
          oldestArchiveId = null;
          isLoadingOlderMessages.value = false;
          await loadMessages(spaceId, channelId);
          return;
        }
      }
      if (isCurrentRequest()) actionError.value = normalizeError(e);
    } finally {
      if (isCurrentRequest()) isLoadingOlderMessages.value = false;
    }
  }

  async function ensureMessageLoaded(messageId: string): Promise<boolean> {
    if (findMessageById(messages.value, messageId)) return true;
    const client = xmppClient.value;
    const channelId = activeChannelId.value;
    if (!client || !channelId || !session.value || !("queryMamPage" in client)) return false;
    const roomJid = roomJidForChannel(channelId);
    if (!roomJid) return false;

    let before = oldestArchiveId;
    while (before && hasOlderMessages.value && !findMessageById(messages.value, messageId)) {
      const requestId = messageRequestId;
      const spaceId = activeSpaceId.value ?? "";
      const previousBefore = before;
      let page: Awaited<ReturnType<typeof client.queryMamPage>>;
      try {
        page = await client.queryMamPage(spaceId, channelId, PAGE_SIZE, { type: "before", before });
      } catch (e) {
        const classified = classifyMamError(e);
        if (isMamCursorNotFound(classified)) {
          // §4.3.4 — stale cursor; the message we were chasing is no longer
          // reachable via the prior cursor. Surface as "not found" so the
          // caller can decide whether to retry from a fresh tail page.
          oldestArchiveId = null;
        }
        return false;
      }
      if (
        requestId !== messageRequestId ||
        xmppClient.value !== client ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        return false;
      }
      const advanced = advanceCursorWithBeforePage({
        prior: { oldestArchiveId: previousBefore, hasOlderMessages: hasOlderMessages.value },
        page,
      });
      oldestArchiveId = advanced.next.oldestArchiveId;
      hasOlderMessages.value = advanced.next.hasOlderMessages;
      const withoutQueued = stripQueuedSelfMessages(messages.value);
      messages.value = appendQueuedMessages(
        buildTimelineFromMamResults(page.messages, withoutQueued),
        roomJid,
      );
      if (findMessageById(messages.value, messageId)) return true;
      if (advanced.stalled || advanced.next.hasOlderMessages === false) break;
      before = advanced.next.oldestArchiveId;
    }
    return !!findMessageById(messages.value, messageId);
  }

  // Backfill a thread via XEP-0313 MAM filtered by thread id. Returns every
  // archived reply whose `<thread>` element matches `threadId`. The thread
  // root does not carry `<thread>` (threads start when someone replies into
  // one), so MAM-by-thread never includes it — the panel resolves the root
  // separately from the loaded channel window.
  async function backfillThread(threadId: string): Promise<void> {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value;
    const channelId = activeChannelId.value;
    if (!client || !channelId || !threadId || !session.value) return;
    const requestId = messageRequestId;
    let page: { messages: LiveRoomMessage[]; firstArchiveId?: string; complete?: boolean } | null = null;
    let results: LiveRoomMessage[] = [];
    try {
      page = "queryMamThreadPage" in client
        ? await client.queryMamThreadPage(spaceId ?? "", channelId, threadId, PAGE_SIZE, { type: "latest" })
        : null;
      results = page ? page.messages : await client.queryMamByThread(spaceId ?? "", channelId, threadId, PAGE_SIZE);
    } catch (e) {
      if (
        xmppClient.value === client &&
        (activeSpaceId.value ?? "") === (spaceId ?? "") &&
        activeChannelId.value === channelId &&
        requestId === messageRequestId
      ) {
        threadHasOlder.value = { ...threadHasOlder.value, [threadId]: false };
        actionError.value = normalizeError(e);
      }
      return;
    }
    if (
      xmppClient.value !== client ||
      (activeSpaceId.value ?? "") !== (spaceId ?? "") ||
      activeChannelId.value !== channelId ||
      requestId !== messageRequestId
    ) {
      return;
    }
    const cursor = threadCursorFromLatestPage({
      page,
      pageSize: PAGE_SIZE,
      fallbackMessages: page ? undefined : results,
    });
    if (cursor.oldestId !== undefined) oldestThreadArchiveIds.set(threadId, cursor.oldestId);
    threadHasOlder.value = { ...threadHasOlder.value, [threadId]: cursor.hasOlder };
    const next = buildTimelineFromMamResults(results, messages.value);
    if (
      next.length === messages.value.length &&
      next.every((message, index) => message === messages.value[index])
    ) {
      return;
    }
    messages.value = next;
  }

  async function loadOlderThreadMessages(threadId: string): Promise<void> {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value;
    const channelId = activeChannelId.value;
    const before = oldestThreadArchiveIds.get(threadId);
    if (!client || !channelId || !threadId || !before || loadingOlderThreadIds.value.has(threadId)) return;
    if (!("queryMamThreadPage" in client)) return;
    const requestId = messageRequestId;
    const isCurrentRequest = () =>
      requestId === messageRequestId &&
      xmppClient.value === client &&
      (activeSpaceId.value ?? "") === (spaceId ?? "") &&
      activeChannelId.value === channelId;
    loadingOlderThreadIds.value = new Set([...loadingOlderThreadIds.value, threadId]);
    try {
      const page = await client.queryMamThreadPage(spaceId ?? "", channelId, threadId, PAGE_SIZE, { type: "before", before });
      if (!isCurrentRequest()) return;
      const advanced = advanceThreadCursor({
        prior: { oldestId: before },
        page,
      });
      if (advanced.oldestId !== undefined) oldestThreadArchiveIds.set(threadId, advanced.oldestId);
      threadHasOlder.value = { ...threadHasOlder.value, [threadId]: advanced.hasOlder };
      messages.value = buildTimelineFromMamResults(page.messages, messages.value);
    } catch (e) {
      const classified = classifyMamError(e);
      if (isMamCursorNotFound(classified)) {
        // §4.3.4 — drop stale thread cursor and let the next backfill restart
        // from the latest page.
        oldestThreadArchiveIds.delete(threadId);
        threadHasOlder.value = { ...threadHasOlder.value, [threadId]: false };
      } else if (isCurrentRequest()) {
        actionError.value = normalizeError(e);
      }
    } finally {
      const next = new Set(loadingOlderThreadIds.value);
      next.delete(threadId);
      loadingOlderThreadIds.value = next;
    }
  }

  function reset() {
    messageRequestId++;
    initialLatestPagePinned = false;
    oldestArchiveId = null;
    hasOlderMessages.value = true;
    isLoadingOlderMessages.value = false;
    isLoadingMessages.value = false;
    loadingOlderThreadIds.value = new Set();
    threadHasOlder.value = {};
    oldestThreadArchiveIds.clear();
  }

  function currentRequestId(): number {
    return messageRequestId;
  }

  function markInitialLatestPagePinned() {
    initialLatestPagePinned = true;
  }

  return {
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    loadingOlderThreadIds,
    threadHasOlder,
    loadMessages,
    loadOlderMessages,
    ensureMessageLoaded,
    backfillThread,
    loadOlderThreadMessages,
    reset,
    currentRequestId,
    markInitialLatestPagePinned,
  };
}
