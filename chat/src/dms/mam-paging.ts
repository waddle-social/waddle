import { nextTick, ref, type Ref } from "vue";
import type { BrowserXmppClient, LiveDmMessage } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageById } from "@/lib/message-ids";
import {
  isTopPinnedScrollDirection,
  type ScrollDirectionMode,
} from "@/lib/scroll-direction";
import { buildDmTimelineFromMamResults } from "@/dms/message-timeline-state";
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

const PAGE_SIZE = 100;

type UseDmMamPagingDeps = {
  session: Ref<WaddleSession | null>;
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
  firstUnseenId: Ref<string | null>;
  loadErrorPeerJid: Ref<string | null>;
  loadErrorMessage: Ref<string>;
  timelineEl: Ref<HTMLDivElement | null>;
  scrollDirection: Ref<ScrollDirectionMode>;
  pinnedEdgeScroller: { disconnect: () => void };
  actionError: Ref<string>;
  clearActionError: () => void;
  pendingEchoClientIds: Set<string>;
  appendQueuedMessages: (timeline: TimelineMessage[], peerJid: string) => TimelineMessage[];
  scrollToPinnedEdgeAndPin: () => Promise<boolean>;
  isFeedVisible: (m: TimelineMessage) => boolean;
  persistLastSeen: (peerJid: string, messageId: string) => void;
  dmLoadErrorMessage: (peerJid: string, opts?: { queuedOnly?: boolean }) => string;
};

export function useDmMamPaging(deps: UseDmMamPagingDeps) {
  const {
    session,
    xmppClient,
    activePeerJid,
    messages,
    firstUnseenId,
    loadErrorPeerJid,
    loadErrorMessage,
    timelineEl,
    scrollDirection,
    pinnedEdgeScroller,
    actionError,
    clearActionError,
    pendingEchoClientIds,
    appendQueuedMessages,
    scrollToPinnedEdgeAndPin,
    isFeedVisible,
    persistLastSeen,
    dmLoadErrorMessage,
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
    mamResults: LiveDmMessage[],
    existing: TimelineMessage[] = [],
  ): TimelineMessage[] {
    if (!session.value) return existing;
    return buildDmTimelineFromMamResults({
      session: session.value,
      mamResults,
      existing,
    });
  }

  async function loadMessages(peerJid: string, unreadAtLoad = 0) {
    if (!session.value) return;
    const requestId = ++messageRequestId;
    initialLatestPagePinned = false;
    isLoadingMessages.value = true;
    isLoadingOlderMessages.value = false;
    hasOlderMessages.value = true;
    oldestArchiveId = null;
    pinnedEdgeScroller.disconnect();
    firstUnseenId.value = null;
    clearActionError();
    loadErrorPeerJid.value = null;
    loadErrorMessage.value = "";
    pendingEchoClientIds.clear();
    messages.value = appendQueuedMessages([], peerJid);

    try {
      const page = xmppClient.value && "queryPersonalMamPage" in xmppClient.value
        ? await xmppClient.value.queryPersonalMamPage(peerJid, PAGE_SIZE, { type: "latest" })
        : null;
      const mamResults = page
        ? page.messages
        : xmppClient.value
          ? await xmppClient.value.queryPersonalMam(peerJid, PAGE_SIZE)
          : [];
      if (requestId !== messageRequestId || activePeerJid.value !== peerJid) return;
      loadErrorPeerJid.value = null;
      loadErrorMessage.value = "";
      const cursor = cursorFromLatestPage({
        page,
        pageSize: PAGE_SIZE,
        fallbackMessages: page ? undefined : mamResults,
      });
      oldestArchiveId = cursor.oldestArchiveId;
      hasOlderMessages.value = cursor.hasOlderMessages;
      const timeline = buildTimelineFromMamResults(mamResults);
      const timelineWithQueue = appendQueuedMessages(timeline, peerJid);
      messages.value = timelineWithQueue;
      if (requestId === messageRequestId) isLoadingMessages.value = false;

      const feedTimeline = timelineWithQueue.filter(isFeedVisible);
      const mdsKey = mdsChatKey(barePeerJid(peerJid));
      const displayed = latestDisplayedStateOnTimeline(
        feedTimeline,
        getMdsDisplayedCandidates(mdsKey),
      );
      firstUnseenId.value = firstUnseenIdAfterDisplayedState(
        feedTimeline,
        firstUnseenIdFromUnreadCount(feedTimeline, unreadAtLoad),
        displayed,
      );
      if (displayed) setMdsDisplayed(mdsKey, displayed);
      const pinned = await scrollToPinnedEdgeAndPin();
      if (!pinned || requestId !== messageRequestId || activePeerJid.value !== peerJid) return;
      initialLatestPagePinned = true;
      const newest = [...timelineWithQueue].reverse().find(isFeedVisible);
      if (newest) persistLastSeen(peerJid, newest.id);
    } catch {
      if (requestId === messageRequestId) {
        console.warn("Could not load DM conversation");
        const queuedOnly = appendQueuedMessages([], peerJid);
        messages.value = queuedOnly;
        loadErrorPeerJid.value = peerJid;
        loadErrorMessage.value = dmLoadErrorMessage(peerJid, { queuedOnly: queuedOnly.length > 0 });
        actionError.value = loadErrorMessage.value;
        isLoadingMessages.value = false;
      }
    }
  }

  async function loadOlderMessages() {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    const before = oldestArchiveId;
    if (
      !client ||
      !peerJid ||
      !before ||
      !initialLatestPagePinned ||
      !hasOlderMessages.value ||
      isLoadingOlderMessages.value
    ) {
      return;
    }
    if (!("queryPersonalMamPage" in client)) return;
    const requestId = messageRequestId;
    const isCurrentRequest = () =>
      requestId === messageRequestId &&
      xmppClient.value === client &&
      activePeerJid.value === peerJid;
    const el = timelineEl.value;
    const previousHeight = el?.scrollHeight ?? 0;
    const previousTop = el?.scrollTop ?? 0;
    isLoadingOlderMessages.value = true;
    try {
      const page = await client.queryPersonalMamPage(peerJid, PAGE_SIZE, { type: "before", before });
      if (!isCurrentRequest()) return;
      const advanced = advanceCursorWithBeforePage({
        prior: { oldestArchiveId, hasOlderMessages: hasOlderMessages.value },
        page,
      });
      oldestArchiveId = advanced.next.oldestArchiveId;
      hasOlderMessages.value = advanced.next.hasOlderMessages;
      const withoutQueued = stripQueuedSelfMessages(messages.value);
      messages.value = appendQueuedMessages(buildTimelineFromMamResults(page.messages, withoutQueued), peerJid);
      await nextTick();
      if (el && !isTopPinnedScrollDirection(scrollDirection.value)) {
        el.scrollTop = previousTop + (el.scrollHeight - previousHeight);
      }
    } catch (e) {
      const classified = classifyMamError(e);
      if (isMamCursorNotFound(classified)) {
        // XEP-0313 §4.3.4 — stale cursor. Drop and re-fetch the tail page.
        // Explicit reset before the await is necessary because `loadMessages`
        // bumps `messageRequestId`; the `finally` block guards on
        // `isCurrentRequest()` and would otherwise leave the flag stuck true.
        if (isCurrentRequest()) {
          oldestArchiveId = null;
          isLoadingOlderMessages.value = false;
          try {
            await loadMessages(peerJid);
          } catch (recoveryError) {
            console.warn("MAM §4.3.4 recovery failed", recoveryError);
          }
          return;
        }
      }
      if (isCurrentRequest()) {
        console.warn("Could not load older DM messages");
        actionError.value = dmLoadErrorMessage(peerJid);
      }
    } finally {
      if (isCurrentRequest()) isLoadingOlderMessages.value = false;
    }
  }

  async function ensureMessageLoaded(messageId: string): Promise<boolean> {
    if (findMessageById(messages.value, messageId)) return true;
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid || !("queryPersonalMamPage" in client)) return false;

    let before = oldestArchiveId;
    while (before && hasOlderMessages.value && !findMessageById(messages.value, messageId)) {
      const requestId = messageRequestId;
      const previousBefore = before;
      let page: Awaited<ReturnType<typeof client.queryPersonalMamPage>>;
      try {
        page = await client.queryPersonalMamPage(peerJid, PAGE_SIZE, { type: "before", before });
      } catch (e) {
        const classified = classifyMamError(e);
        if (isMamCursorNotFound(classified)) {
          // §4.3.4 — drop the cursor AND collapse hasOlderMessages so the
          // UI's "load older" sentinel doesn't keep prompting paging that
          // would early-return on !before. A fresh loadMessages() resets
          // both to their initial state.
          oldestArchiveId = null;
          hasOlderMessages.value = false;
        } else if (
          requestId === messageRequestId &&
          xmppClient.value === client &&
          activePeerJid.value === peerJid
        ) {
          // Non-§4.3.4 failure: surface to the caller instead of silently
          // returning false.
          actionError.value = dmLoadErrorMessage(peerJid);
        }
        return false;
      }
      if (
        requestId !== messageRequestId ||
        xmppClient.value !== client ||
        activePeerJid.value !== peerJid
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
      messages.value = appendQueuedMessages(buildTimelineFromMamResults(page.messages, withoutQueued), peerJid);
      if (findMessageById(messages.value, messageId)) return true;
      if (advanced.stalled || advanced.next.hasOlderMessages === false) break;
      before = advanced.next.oldestArchiveId;
    }
    return !!findMessageById(messages.value, messageId);
  }

  // Backfill a DM thread via XEP-0313 MAM filtered by thread id against the
  // account archive (`with=peer` + thread field). Mirrors the channel
  // `backfillThread`: the thread root carries no `<thread>` (threads start on
  // the first reply), so MAM-by-thread never returns it — the panel resolves
  // the root from the loaded DM window separately.
  async function backfillThread(threadId: string): Promise<void> {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid || !threadId || !session.value) return;
    if (!("queryPersonalMamThreadPage" in client) && !("queryPersonalMamByThread" in client)) return;
    const requestId = messageRequestId;
    const isCurrentRequest = () =>
      requestId === messageRequestId &&
      xmppClient.value === client &&
      activePeerJid.value === peerJid;
    let page: { messages: LiveDmMessage[]; firstArchiveId?: string; complete?: boolean } | null = null;
    let results: LiveDmMessage[] = [];
    try {
      page = "queryPersonalMamThreadPage" in client
        ? await client.queryPersonalMamThreadPage(peerJid, threadId, PAGE_SIZE, { type: "latest" })
        : null;
      results = page ? page.messages : await client.queryPersonalMamByThread(peerJid, threadId, PAGE_SIZE);
    } catch (e) {
      if (isCurrentRequest()) {
        threadHasOlder.value = { ...threadHasOlder.value, [threadId]: false };
        actionError.value = dmLoadErrorMessage(peerJid);
        console.warn("Could not backfill DM thread", e);
      }
      return;
    }
    if (!isCurrentRequest()) return;
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
    const peerJid = activePeerJid.value;
    const before = oldestThreadArchiveIds.get(threadId);
    if (!client || !peerJid || !threadId || !before || loadingOlderThreadIds.value.has(threadId)) return;
    if (!("queryPersonalMamThreadPage" in client)) return;
    const requestId = messageRequestId;
    const isCurrentRequest = () =>
      requestId === messageRequestId &&
      xmppClient.value === client &&
      activePeerJid.value === peerJid;
    loadingOlderThreadIds.value = new Set([...loadingOlderThreadIds.value, threadId]);
    try {
      const page = await client.queryPersonalMamThreadPage(peerJid, threadId, PAGE_SIZE, { type: "before", before });
      if (!isCurrentRequest()) return;
      const advanced = advanceThreadCursor({ prior: { oldestId: before }, page });
      if (advanced.oldestId !== undefined) oldestThreadArchiveIds.set(threadId, advanced.oldestId);
      threadHasOlder.value = { ...threadHasOlder.value, [threadId]: advanced.hasOlder };
      messages.value = buildTimelineFromMamResults(page.messages, messages.value);
    } catch (e) {
      const classified = classifyMamError(e);
      if (isMamCursorNotFound(classified)) {
        // §4.3.4 — drop the stale thread cursor; the next backfill restarts
        // from the latest page. Guarded so a stale completion after a peer
        // switch can't corrupt the new conversation's thread state.
        if (isCurrentRequest()) {
          oldestThreadArchiveIds.delete(threadId);
          threadHasOlder.value = { ...threadHasOlder.value, [threadId]: false };
        }
      } else if (isCurrentRequest()) {
        actionError.value = dmLoadErrorMessage(peerJid);
      }
    } finally {
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
