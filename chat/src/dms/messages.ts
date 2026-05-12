import { computed, nextTick, ref, watch, type Ref } from "vue";
import {
  type DeliveryStatus,
  type TimelineMessage,
} from "@/lib/chat-ui";
import type {
  BrowserXmppClient,
  ChatStateType,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  MessageSearchResult,
  SessionLifecycleEvent,
} from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import {
  findMessageById,
  matchMessageId,
  mergeMessageIds,
} from "@/lib/message-ids";
import { findMessageElementById } from "@/lib/message-targeting";
import { isTopPinnedScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import { dmKey, setLastSeen } from "@/lib/last-seen-store";
import {
  listQueuedDmMessages,
} from "@/lib/outbound-queue-store";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import {
  fromLiveDmMessage,
  isSameDmCorrectionSender,
  queuedDmMessageToTimeline,
  retractDmTimelineMessage,
} from "@/dms/message-timeline-state";
import { useDmMamPaging } from "@/dms/mam-paging";
import { useChatSend } from "@/dms/chat-send";
import { useDmMessageActions } from "@/dms/message-actions";
import { useDmLiveMerge } from "@/dms/live-merge";

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
  const typingUsers = ref<string[]>([]);
  const searchResults = ref<MessageSearchResult[]>([]);
  const isSearching = ref(false);
  const firstUnseenId = ref<string | null>(null);
  const loadErrorPeerJid = ref<string | null>(null);
  const loadErrorMessage = ref("");
  const latestRemoteMessageId = computed<string | null>(() => {
    for (let i = messages.value.length - 1; i >= 0; i--) {
      const m = messages.value[i];
      if (!m || m.isSelf || m.isRetracted) continue;
      return m.id;
    }
    return null;
  });

  let searchRequestId = 0;
  let lastChatState: ChatStateType = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();

  function queuedMessagesForPeer(peerJid: string): TimelineMessage[] {
    const currentSession = session.value;
    if (!currentSession) return [];
    const queued = listQueuedDmMessages(barePeerJid(currentSession.jid), barePeerJid(peerJid));
    for (const message of queued) pendingEchoClientIds.add(message.id);
    return queued.map((message) => queuedDmMessageToTimeline(currentSession, message));
  }

  function appendQueuedMessages(timeline: TimelineMessage[], peerJid: string): TimelineMessage[] {
    const queued = queuedMessagesForPeer(peerJid).filter((message) => !findMessageById(timeline, message.id));
    return queued.length > 0 ? [...timeline, ...queued] : timeline;
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

  function addTypingUser(nick: string) {
    if (!typingUsers.value.includes(nick)) {
      typingUsers.value = [...typingUsers.value, nick];
    }
    const existing = typingTimers.get(nick);
    if (existing) clearTimeout(existing);
    typingTimers.set(nick, setTimeout(() => removeTypingUser(nick), 5000));
  }

  function removeTypingUser(nick: string) {
    const timer = typingTimers.get(nick);
    if (timer) {
      clearTimeout(timer);
      typingTimers.delete(nick);
    }
    if (typingUsers.value.includes(nick)) {
      typingUsers.value = typingUsers.value.filter((n) => n !== nick);
    }
  }

  function clearTypingState() {
    for (const timer of typingTimers.values()) clearTimeout(timer);
    typingTimers.clear();
    typingUsers.value = [];
    lastChatState = "active";
    if (composingTimeout) {
      clearTimeout(composingTimeout);
      composingTimeout = null;
    }
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
      if (composingTimeout) {
        clearTimeout(composingTimeout);
        composingTimeout = null;
      }
      lastChatState = "active";
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
    persistLastSeen: (peerJid, messageId) => setLastSeen(dmKey(barePeerJid(peerJid)), messageId),
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
    persistLastSeen: (peerJid, messageId) => setLastSeen(dmKey(barePeerJid(peerJid)), messageId),
    dmLoadErrorMessage,
  });
  const {
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    loadOlderMessages,
    ensureMessageLoaded,
  } = paging;

  // DM-load entry point. Search state lives in this orchestrator, so we
  // reset it before delegating to the MAM-paging composable — keeping the
  // pre-#188 invariant that switching peers invalidates in-flight searches
  // and clears stale results.
  async function loadMessages(peerJid: string, unreadAtLoad = 0) {
    searchRequestId++;
    searchResults.value = [];
    isSearching.value = false;
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


  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    void xmppClient.value.sendDmDisplayed(activePeerJid.value, targetId).catch(() => undefined);
  }

  function notifyComposing() {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    if (lastChatState !== "composing") {
      lastChatState = "composing";
      void client.sendDmChatState(peerJid, "composing").catch(() => undefined);
    }
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (xmppClient.value !== client || activePeerJid.value !== peerJid) return;
      lastChatState = "paused";
      void client.sendDmChatState(peerJid, "paused").catch(() => undefined);
    }, 3000);
  }

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    clearActionError();
    try {
      const results = await client.searchDmMessages(peerJid, trimmed);
      if (requestId === searchRequestId && xmppClient.value === client && activePeerJid.value === peerJid) {
        searchResults.value = results;
      }
    } catch (e) {
      if (requestId === searchRequestId) {
        searchResults.value = [];
        actionError.value = normalizeError(e);
      }
    } finally {
      if (requestId === searchRequestId) isSearching.value = false;
    }
  }

  function clearSearch() {
    searchRequestId++;
    searchResults.value = [];
    isSearching.value = false;
  }

  function clearMessages() {
    paging.reset();
    searchRequestId++;
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    messages.value = [];
    searchResults.value = [];
    isSearching.value = false;
    firstUnseenId.value = null;
    clearTypingState();
  }

  function disconnect() {
    paging.reset();
    searchRequestId++;
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    isSearching.value = false;
    searchResults.value = [];
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
        if (peerJid && newest) setLastSeen(dmKey(barePeerJid(peerJid)), newest.id);
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
