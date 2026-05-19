import { nextTick, ref, watch, type Ref } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import type {
  BrowserXmppClient,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  SessionLifecycleEvent,
} from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { findMessageById } from "@/lib/message-ids";
import { compareTimelineMessages } from "@/lib/timeline-timestamps";
import { findMessageElementById } from "@/lib/message-targeting";
import { isTopPinnedScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import {
  listQueuedDmMessages,
} from "@/lib/outbound-queue-store";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import { queuedDmMessageToTimeline } from "@/dms/message-timeline-state";
import { useDmMamPaging } from "@/dms/mam-paging";
import { useChatSend } from "@/dms/chat-send";
import { useDmMessageActions } from "@/dms/message-actions";
import { useDmLiveMerge } from "@/dms/live-merge";
import { useDmChatStates } from "@/dms/chat-states";
import { useDmReadMarkers } from "@/dms/read-markers";
import { useDmMessageSearch } from "@/dms/message-search";

export function useDirectMessages(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activePeerJid: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const { mode: scrollDirection } = useScrollDirectionPreference();
  const peerNameFromJid = (jid: string) => barePeerJid(jid).split("@")[0] ?? "unknown";
  const dmLoadErrorMessage = (peerJid: string, opts: { queuedOnly?: boolean } = {}) => {
    const username = peerNameFromJid(peerJid).trim();
    const target = username ? `@${username}` : "this chat";
    return opts.queuedOnly
      ? `Could not load ${target} history. Showing queued messages only. Check the connection and try again.`
      : `Could not load ${target}. Check the connection and try again.`;
  };
  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const timelineEdgeScroller: Ref<((mode: ScrollDirectionMode) => boolean | Promise<boolean>) | null> = ref(null);
  const pinnedEdgeScroller = createPinnedEdgeScroller({
    element: timelineEl,
    mode: scrollDirection,
    virtualScroll: timelineEdgeScroller,
  });
  const loadErrorPeerJid = ref<string | null>(null);
  const loadErrorMessage = ref("");

  const chatStates = useDmChatStates({ xmppClient, activePeerJid });
  const {
    typingUsers,
    addTypingUser,
    removeTypingUser,
    clearTypingState,
    notifyComposing,
  } = chatStates;

  const readMarkers = useDmReadMarkers({ xmppClient, activePeerJid, messages });
  const {
    firstUnseenId,
    latestRemoteMessageId,
    markDisplayed,
    persistLastSeen,
  } = readMarkers;

  const messageSearch = useDmMessageSearch({
    xmppClient,
    activePeerJid,
    actionError,
    clearActionError,
    normalizeError,
  });
  const {
    searchResults,
    isSearching,
    searchMessages,
    clearSearch,
  } = messageSearch;

  function queuedMessagesForPeer(peerJid: string): TimelineMessage[] {
    const currentSession = session.value;
    if (!currentSession) return [];
    const queued = listQueuedDmMessages(barePeerJid(currentSession.jid), barePeerJid(peerJid));
    for (const message of queued) pendingEchoClientIds.add(message.id);
    return queued.map((message) => queuedDmMessageToTimeline(currentSession, message));
  }

  function appendQueuedMessages(timeline: TimelineMessage[], peerJid: string): TimelineMessage[] {
    const queued = queuedMessagesForPeer(peerJid).filter((message) => !findMessageById(timeline, message.id));
    if (queued.length === 0) return timeline;
    // Both inputs are individually sorted (timeline via compareTimelineMessages,
    // queued via outbound-queue-store's createdAt+id sort) and the queued rows
    // are all in the pending bucket — but a concat alone can mis-interleave if
    // `timeline` already contains a still-pending optimistic row whose
    // client-wall-clock createdAt sits inside the queued range. Sort the merged
    // array through the canonical total-order comparator so the pending tail is
    // internally consistent regardless of clock drift.
    return [...timeline, ...queued].sort(compareTimelineMessages);
  }

  async function scrollToPinnedEdge() {
    await pinnedEdgeScroller.scrollToPinnedEdge();
  }

  async function scrollToPinnedEdgeAndPin() {
    return pinnedEdgeScroller.scrollToPinnedEdge({ settle: true });
  }

  async function scrollFirstUnseenIntoView(messageId: string) {
    if (isTopPinnedScrollDirection(scrollDirection.value)) {
      await scrollToPinnedEdgeAndPin();
      return;
    }
    await nextTick();
    await nextTick();
    const el = timelineEl.value;
    if (!el) return;
    // Prefer the divider so it stays on-screen; message-level fallback only
    // if the divider hasn't rendered yet.
    const divider = el.querySelector("[data-new-messages-divider]");
    if (divider && typeof (divider as HTMLElement).scrollIntoView === "function") {
      (divider as HTMLElement).scrollIntoView({ block: "start" });
      return;
    }
    const target = findMessageElementById(el, messageId);
    if (target && typeof (target as HTMLElement).scrollIntoView === "function") {
      (target as HTMLElement).scrollIntoView({ block: "start" });
    }
  }

  async function alignTimelineToPreference() {
    if (firstUnseenId.value) {
      await scrollFirstUnseenIntoView(firstUnseenId.value);
      return;
    }
    await scrollToPinnedEdgeAndPin();
  }

  function isFeedVisible(m: TimelineMessage): boolean {
    return !m.threadId || m.id === m.threadId;
  }

  const send = useChatSend({
    session,
    xmppClient,
    activePeerJid,
    messages,
    draft,
    actionError,
    clearActionError,
    normalizeError,
    scrollToPinnedEdgeAndPin,
    onSendComplete: (result, peerJid, isStillActive) => {
      if (!isStillActive) {
        // Client swap or peer change happened during the send. Don't
        // emit XEP-0085 "active" through the new session targeting the
        // old peer.
        return;
      }
      chatStates.resetOnSend();
      if (result?.state === "sending" && xmppClient.value) {
        void xmppClient.value.sendDmChatState(peerJid, "active").catch(() => undefined);
      }
    },
  });
  const {
    isSending,
    uploadProgress,
    pendingEchoClientIds,
    sendMessage,
    editMessage,
    onMessageAck,
    onMessageQueueStatus,
    onMessageDeliveryFailure,
  } = send;

  const liveMerge = useDmLiveMerge({
    session,
    messages,
    activePeerJid,
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
    isFeedVisible,
  });
  const { applyDisplayed, applyReaction } = liveMerge;

  const actions = useDmMessageActions({
    session,
    xmppClient,
    activePeerJid,
    messages,
    actionError,
    clearActionError,
    normalizeError,
    applyReaction,
  });
  const {
    toggleReaction,
    retractMessage,
    invokeExtensionAction,
  } = actions;

  const paging = useDmMamPaging({
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
  });
  const {
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    loadOlderMessages,
    ensureMessageLoaded,
  } = paging;

  // DM-load entry point. The MAM-paging composable owns paging state; the
  // message-search composable owns search state. Reset both before
  // delegating so switching peers invalidates in-flight searches and
  // clears stale results.
  async function loadMessages(peerJid: string, unreadAtLoad = 0) {
    messageSearch.reset();
    await paging.loadMessages(peerJid, unreadAtLoad);
  }

  /** On a fresh session (SM resume failed), re-fetch MAM to close any gap
   *  for the currently-open conversation. Local optimistic sends are
   *  preserved across the reload so the user keeps retry affordances. */
  function onSessionLifecycle(event: SessionLifecycleEvent) {
    if (event.type !== "fresh") return;
    const peerJid = activePeerJid.value;
    if (!peerJid) return;
    if (messages.value.length === 0) return;
    const preserved = messages.value.filter(
      (m) =>
        m.isSelf && (
          m.deliveryStatus === "queued"
          || m.deliveryStatus === "sending"
          || m.deliveryStatus === "failed"
        ),
    );
    void (async () => {
      await loadMessages(peerJid);
      if (preserved.length === 0 || activePeerJid.value !== peerJid) return;
      const toAppend = preserved.filter((m) => !findMessageById(messages.value, m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }



  function clearMessages() {
    paging.reset();
    messageSearch.reset();
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    messages.value = [];
    firstUnseenId.value = null;
    clearTypingState();
  }

  function disconnect() {
    paging.reset();
    messageSearch.reset();
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    firstUnseenId.value = null;
    clearTypingState();
  }

  function onIncomingMessage(msg: LiveDmMessage) {
    if (!session.value || !activePeerJid.value || msg.peerJid !== activePeerJid.value) return;
    removeTypingUser(msg.nick);
    liveMerge.handleIncomingMessage(msg);
  }

  function onChatState(event: DmChatStateEvent) {
    if (!activePeerJid.value || event.peerJid !== activePeerJid.value) return;
    const peerName = peerNameFromJid(event.peerJid);
    if (event.state === "composing") addTypingUser(peerName);
    else removeTypingUser(peerName);
  }

  function onDisplayed(event: DmDisplayedEvent) {
    if (!activePeerJid.value || event.peerJid !== activePeerJid.value) return;
    applyDisplayed(event.messageId, peerNameFromJid(event.peerJid));
  }

  function onReaction(event: DmReactionEvent) {
    if (!activePeerJid.value || event.peerJid !== activePeerJid.value) return;
    // Attribute the reaction to whoever sent the stanza, not the
    // conversation key. For self-sent carbon-forwarded reactions
    // `peerJid` is normalized to the recipient, so deriving the nick
    // from `peerJid` would wrongly credit the partner.
    applyReaction(event.messageId, peerNameFromJid(event.reactorJid), event.emojis);
  }

  watch(scrollDirection, () => {
    void alignTimelineToPreference();
  });

  watch(
    [timelineEl, timelineEdgeScroller],
    async ([el, edgeScroller]) => {
      if (!el || !edgeScroller || isLoadingMessages.value) return;
      if (!messages.value.some(isFeedVisible)) return;
      const requestId = paging.currentRequestId();
      const peerJid = activePeerJid.value;
      const pinned = await scrollToPinnedEdgeAndPin();
      if (
        pinned &&
        requestId === paging.currentRequestId() &&
        activePeerJid.value === peerJid &&
        messages.value.some(isFeedVisible)
      ) {
        paging.markInitialLatestPagePinned();
        const newest = [...messages.value].reverse().find(isFeedVisible);
        if (peerJid && newest) persistLastSeen(peerJid, newest.id);
      }
    },
    { flush: "post" },
  );

  return {
    messages,
    firstUnseenId,
    draft,
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    isSending,
    typingUsers,
    timelineEl,
    timelineEdgeScroller,
    searchResults,
    isSearching,
    loadErrorPeerJid,
    loadErrorMessage,
    loadMessages,
    loadOlderMessages,
    ensureMessageLoaded,
    sendMessage,
    uploadProgress,
    editMessage,
    retractMessage,
    invokeExtensionAction,
    toggleReaction,
    markDisplayed,
    notifyComposing,
    searchMessages,
    clearSearch,
    clearMessages,
    disconnect,
    onIncomingMessage,
    onChatState,
    onDisplayed,
    onReaction,
    onMessageQueueStatus,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
    scrollToPinnedEdge,
    isPinnedAtEdge: pinnedEdgeScroller.isPinnedAtEdge,
    latestRemoteMessageId,
  };
}
