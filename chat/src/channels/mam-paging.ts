import { nextTick, ref, type Ref } from "vue";
import type { ChannelSummary } from "@/lib/chat-types";
import { isForumChannel } from "@/lib/channel-types";
import type { WaddleSession } from "@/lib/server-auth";
import type {
  BrowserXmppClient,
  LiveRoomMessage,
} from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { TimelineLoadResult } from "@/lib/timeline-load-result";
import { findMessageById } from "@/lib/message-ids";
import {
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import {
  buildChannelTimelineFromMamResults,
  type TimelineBuildOptions,
} from "@/channels/message-timeline-state";
import { applyForumContext, isFeedTimelineMessage } from "@/channels/timeline";
import { insertLiveMessage } from "@/lib/messaging/timeline-insert";
import {
  firstUnseenIdAfterDisplayedState,
  firstUnseenIdFromUnreadCount,
  latestDisplayedStateOnTimeline,
} from "@/lib/displayed-state";
import { getMdsDisplayedCandidates, mdsChatKey, setMdsDisplayed } from "@/lib/last-seen-store";
import {
  advanceCursorWithBeforePage,
  advanceThreadCursor,
  classifyMamError,
  cursorFromLatestPage,
  isMamCursorNotFound,
  stripQueuedSelfMessages,
  threadCursorFromLatestPage,
} from "@/lib/xmpp/mam";
import { hydratePinnedRoom, pinnedRoomsEpoch } from "@/stores/pinned-messages";
import type { ChannelLoadIntent } from "@/channels/room-access";

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
  pinnedEdgeScroller: { cancelSettleLock: () => void };
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  pendingEchoClientIds: Set<string>;
  appendQueuedMessages: (timeline: TimelineMessage[], roomJid: string) => TimelineMessage[];
  roomJidForChannel: (channelId: string) => string | null;
  isRoomAccessRequired: (roomJid: string) => boolean;
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
    isRoomAccessRequired,
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
    options: { intent?: ChannelLoadIntent } = {},
  ): Promise<TimelineLoadResult> {
    if (!session.value) return "aborted";

    const requestId = ++messageRequestId;
    const roomJid = roomJidForChannel(channelId);
    if (!roomJid) return "aborted";
    initialLatestPagePinned = false;
    isLoadingMessages.value = true;
    isLoadingOlderMessages.value = false;
    hasOlderMessages.value = true;
    oldestArchiveId = null;
    loadingOlderThreadIds.value = new Set();
    threadHasOlder.value = {};
    oldestThreadArchiveIds.clear();
    pinnedEdgeScroller.cancelSettleLock();
    // Reset the divider anchor up-front: a previous conversation's id could
    // coincidentally match a message in the new timeline, and an aborted
    // request (requestId mismatch) would otherwise leave stale state.
    firstUnseenId.value = null;
    clearActionError();
    pendingEchoClientIds.clear();
    messages.value = appendQueuedMessages([], roomJid);

    if (
      options.intent !== "explicit-navigation"
      && isRoomAccessRequired(roomJid)
    ) {
      isLoadingMessages.value = false;
      hasOlderMessages.value = false;
      hydratePinnedRoom(roomJid, []);
      return "loaded";
    }

    try {
      if (
        options.intent === "explicit-navigation"
        && xmppClient.value
        && "retryRoomAccess" in xmppClient.value
      ) {
        await xmppClient.value.retryRoomAccess(spaceId, channelId);
      }

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
        return "aborted";
      }

      const cursor = cursorFromLatestPage({
        page,
        pageSize: PAGE_SIZE,
        fallbackMessages: page ? undefined : mamResults,
      });
      oldestArchiveId = cursor.oldestArchiveId;
      hasOlderMessages.value = cursor.hasOlderMessages;
      // #675: a live message merged into `messages.value` during the
      // queryMamPage await must survive the rebuild. Re-insert each one
      // through the same reconciliation as mergeLiveMessage, so a MAM copy
      // of the same message (XEP-0359 id parity) merges instead of
      // duplicating.
      const liveDuringLoad = stripQueuedSelfMessages(messages.value);
      let rebuilt = buildTimelineFromMamResults(mamResults, metadataSeed, {
        seedExistingOnly: metadataSeed.length > 0,
      });
      for (const live of liveDuringLoad) {
        rebuilt = insertLiveMessage(rebuilt, live, pendingEchoClientIds).messages;
      }
      // One forum-context pass after all re-inserts: nothing observes
      // the intermediate timelines (messages.value is assigned below),
      // so recomputing per insert — as the true live-merge path must —
      // would be pure waste here.
      if (liveDuringLoad.length > 0) {
        rebuilt = applyForumContext(rebuilt, isForumChannel(currentChannel.value));
      }
      const timelineWithQueue = appendQueuedMessages(rebuilt, roomJid);
      messages.value = timelineWithQueue;
      if (requestId === messageRequestId) {
        isLoadingMessages.value = false;
      }

      const feedTimeline = timelineWithQueue.filter(isFeedVisible);
      const mdsKey = mdsChatKey(roomJid);
      const displayed = latestDisplayedStateOnTimeline(
        feedTimeline,
        getMdsDisplayedCandidates(mdsKey),
      );
      // #675: unreadAtLoad was counted before the load; foreign live
      // arrivals during the await extend the timeline tail and are
      // unread too — without counting them the divider lands that many
      // rows too new. When unreadAtLoad is 0 the mid-load arrival
      // behaves like a post-load arrival (no divider).
      const liveUnreadDuringLoad = liveDuringLoad.filter(
        (m) => isFeedVisible(m) && !m.isSelf,
      ).length;
      const unreadForDivider =
        unreadAtLoad > 0 ? unreadAtLoad + liveUnreadDuringLoad : 0;
      firstUnseenId.value = firstUnseenIdAfterDisplayedState(
        feedTimeline,
        firstUnseenIdFromUnreadCount(feedTimeline, unreadForDivider),
        displayed,
      );
      if (displayed) setMdsDisplayed(mdsKey, displayed);
      const pinned = await scrollToPinnedEdgeAndPin();
      if (
        requestId !== messageRequestId ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        // Superseded during the pin step: the timeline was rebuilt, but
        // a newer request owns it now — callers must not react.
        return "aborted";
      }
      if (!pinned) {
        return "loaded";
      }
      initialLatestPagePinned = true;
      const newest = [...timelineWithQueue].reverse().find(isFeedVisible);
      if (newest) persistLastSeen(channelId, newest.id);
      return "loaded";
    } catch (e) {
      if (requestId !== messageRequestId) return "aborted";
      // #675: messages.value already holds the queued rows plus any
      // live message merged during the failed await — keep them
      // instead of resetting to queued-only. The error stays suppressed
      // only for queued self-sends (pre-existing behavior); a live
      // arrival doesn't hide that history failed to load.
      const hasQueuedRows = messages.value.some(
        (m) => m.deliveryStatus === "queued" || m.deliveryStatus === "sending",
      );
      actionError.value = hasQueuedRows ? "" : normalizeError(e);
      isLoadingMessages.value = false;
      return "failed";
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
        // safer than reporting "no more history" to the user. The explicit
        // `isLoadingOlderMessages = false` here is necessary because
        // `loadMessages` bumps `messageRequestId`, so the `finally` block's
        // `isCurrentRequest()` guard will be false by the time it runs.
        // Setting `oldestArchiveId = null` first also blocks any concurrent
        // `loadOlderMessages` from racing past its `!before` guard while we
        // await the tail-page refetch.
        if (isCurrentRequest()) {
          oldestArchiveId = null;
          isLoadingOlderMessages.value = false;
          try {
            await loadMessages(spaceId, channelId);
          } catch (recoveryError) {
            console.warn("MAM §4.3.4 recovery failed", recoveryError);
          }
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
          // reachable via the prior cursor. Drop the cursor AND collapse
          // hasOlderMessages so the UI's "load older" sentinel stops
          // offering a paging entry that would early-return on !before.
          // A fresh loadMessages() resets both to their initial state.
          oldestArchiveId = null;
          hasOlderMessages.value = false;
        } else if (
          requestId === messageRequestId &&
          xmppClient.value === client &&
          (activeSpaceId.value ?? "") === spaceId &&
          activeChannelId.value === channelId
        ) {
          // Non-§4.3.4 failure: surface so the caller (reply jump / pinned
          // jump / search-result open) doesn't see a silent "not found".
          actionError.value = normalizeError(e);
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
        // from the latest page. Guarded by isCurrentRequest() so a late
        // completion after channel switch can't corrupt the new channel's
        // thread state (which a fresh reset() has already cleared).
        if (isCurrentRequest()) {
          oldestThreadArchiveIds.delete(threadId);
          threadHasOlder.value = { ...threadHasOlder.value, [threadId]: false };
        }
      } else if (isCurrentRequest()) {
        actionError.value = normalizeError(e);
      }
    } finally {
      // Guard the cleanup too — a stale completion must not delete a
      // threadId entry from the new channel's loading set, which would
      // unstick a spinner that belongs to a different in-flight request.
      if (isCurrentRequest()) {
        const next = new Set(loadingOlderThreadIds.value);
        next.delete(threadId);
        loadingOlderThreadIds.value = next;
      }
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
