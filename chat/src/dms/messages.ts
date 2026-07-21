import { getCurrentScope, nextTick, onScopeDispose, ref, watch, type Ref } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import type {
  BrowserXmppClient,
  DmConversationScope,
  DmChatStateEvent,
  DmDisplayedEvent,
  DmReactionEvent,
  LiveDmMessage,
  SessionLifecycleEvent,
} from "@/lib/xmpp-client";
import { barePeerJid, dmConversationIdentityKey, jidLocalpart } from "@/lib/xmpp-client";
import { bareJidKey, fullJidIdentityKey, resourceOf } from "@/lib/xmpp/jid";
import type { CatchupConversationFailure } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import {
  displayedStateCanAdvance,
  firstUnseenIdAfterDisplayedState,
} from "@/lib/displayed-state";
import { getMdsDisplayed, mdsChatKey, type MdsDisplayedState } from "@/lib/last-seen-store";
import { findMessageById } from "@/lib/message-ids";
import { mergeQueuedIntoTimeline } from "@/lib/timeline-queue-merge";
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
import { useChatWindowVisibility } from "@/shell/window-visibility";
import { withSpan } from "@/lib/telemetry";
import {
  $dmCallOutcomeAnchor,
  $dmCallStartedAnchor,
  resolveDmCallAnchorInjection,
  resolveDmCallOutcomeInjection,
} from "@/lib/calls/dm-call-anchor";

export function useDirectMessages(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activePeerJid: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
  activeConversationScope?: Readonly<Ref<DmConversationScope | null>>,
) {
  const { mode: scrollDirection } = useScrollDirectionPreference();
  const peerNameFromJid = (jid: string) => jidLocalpart(jid);
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

  const chatStates = useDmChatStates({ xmppClient, activePeerJid, conversationScope });
  const {
    typingUsers,
    addTypingUser,
    removeTypingUser,
    clearTypingState,
    notifyComposing,
  } = chatStates;

  function conversationScope(peerJid: string): DmConversationScope {
    if (activePeerJid.value === peerJid && activeConversationScope?.value) {
      return activeConversationScope.value;
    }
    return xmppClient.value?.isMucPmPeer?.(peerJid) === true ? "muc-occupant" : "account";
  }

  const readMarkers = useDmReadMarkers({
    xmppClient,
    activePeerJid,
    messages,
    conversationScope,
  });
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
    conversationScope,
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
    const queuePeer = dmConversationIdentityKey(
      peerJid,
      () => conversationScope(peerJid) === "muc-occupant",
    );
    const queued = listQueuedDmMessages(
      barePeerJid(currentSession.jid),
      queuePeer,
      conversationScope(peerJid),
    );
    for (const message of queued) pendingEchoClientIds.add(message.id);
    return queued.map((message) => queuedDmMessageToTimeline(currentSession, message));
  }

  function appendQueuedMessages(timeline: TimelineMessage[], peerJid: string): TimelineMessage[] {
    return mergeQueuedIntoTimeline(timeline, queuedMessagesForPeer(peerJid));
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
    conversationScope,
    onSendComplete: (result, peerJid, isStillActive, scope) => {
      if (!isStillActive) {
        // Client swap or peer change happened during the send. Don't
        // emit XEP-0085 "active" through the new session targeting the
        // old peer.
        return;
      }
      chatStates.resetOnSend();
      if (result?.state === "sending" && xmppClient.value) {
        void xmppClient.value.sendDmChatState(peerJid, "active", undefined, scope).catch(() => undefined);
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

  // Surface a "started a call" anchor card the moment a 1:1 call becomes
  // active. The server only enriches the archived `<proceed/>` row, so this
  // synthesized card is the live path; the later MAM-replayed archive row
  // dedups against it by call sid (see `dm-call-anchor.ts`). `.listen` fires
  // only on new publishes, so a stale anchor is never replayed on mount.
  //
  // `useDirectMessages` runs inside component setup in the app (an effect
  // scope is present). Subscribe only within a scope so the listener is always
  // paired with its disposer — never subscribed-and-leaked when the composable
  // is unit-instantiated outside a scope.
  if (getCurrentScope()) {
    const stopDmCallAnchorListener = $dmCallStartedAnchor.listen((anchor) => {
      if (!anchor) return;
      const card = resolveDmCallAnchorInjection(
        anchor,
        activePeerJid.value,
        session.value?.jid ?? null,
      );
      if (card) liveMerge.mergeLiveMessage(card);
    });
    const stopDmCallOutcomeListener = $dmCallOutcomeAnchor.listen((anchor) => {
      if (!anchor) return;
      const card = resolveDmCallOutcomeInjection(
        anchor,
        activePeerJid.value,
        session.value?.jid ?? null,
      );
      if (card) liveMerge.mergeLiveMessage(card);
    });
    onScopeDispose(stopDmCallAnchorListener);
    onScopeDispose(stopDmCallOutcomeListener);
  }

  const actions = useDmMessageActions({
    session,
    xmppClient,
    activePeerJid,
    messages,
    actionError,
    clearActionError,
    normalizeError,
    applyReaction,
    conversationScope,
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
    conversationScope,
  });
  const {
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    loadingOlderThreadIds,
    threadHasOlder,
    loadOlderMessages,
    ensureMessageLoaded,
    backfillThread,
    loadOlderThreadMessages,
  } = paging;

  // DM-load entry point. The MAM-paging composable owns paging state; the
  // message-search composable owns search state. Reset both before
  // delegating so switching peers invalidates in-flight searches and
  // clears stale results.
  async function loadMessages(peerJid: string, unreadAtLoad = 0) {
    messageSearch.reset();
    return withSpan(
      "xmpp.initial_render",
      { "conversation.kind": "dm" },
      () => paging.loadMessages(peerJid, unreadAtLoad),
    );
  }

  /** On a fresh session (SM resume failed), re-fetch MAM to close any gap
   *  for the currently-open conversation — unless the client's reconnect
   *  catch-up covers this peer (#1180): a wholesale reload would then
   *  race the catch-up's merges for messages.value. Cursor paging +
   *  live-merge closes the gap, same choreography as a resumed session. */
  function onSessionLifecycle(event: SessionLifecycleEvent) {
    if (event.type !== "fresh") return;
    const peerJid = activePeerJid.value;
    if (!peerJid) return;
    const scopedOccupants = new Set(event.catchup.dmOccupantJids ?? []);
    const isMucPmPeer = (jid: string) => scopedOccupants.has(jid) || (xmppClient.value?.isMucPmPeer?.(jid) ?? false);
    const peerKey = dmConversationIdentityKey(peerJid, isMucPmPeer);
    if (event.catchup.dmJids.some((jid) => dmConversationIdentityKey(jid, isMucPmPeer) === peerKey)) return;
    refetchTimelineToCloseGap();
  }

  /** Fallback for a covered-but-failed reconnect catch-up (#1180): run
   *  the wholesale reload the lifecycle skip suppressed — serialized
   *  after the failed attempt, so the two fetches can no longer race. */
  function onCatchupFailed(failure: CatchupConversationFailure) {
    if (failure.kind !== "dm") return;
    const peerJid = activePeerJid.value;
    const isMucPmPeer = (jid: string) =>
      (failure.dmScope === "muc-occupant" && jid === failure.key)
      || (xmppClient.value?.isMucPmPeer?.(jid) ?? false);
    if (
      !peerJid
      || dmConversationIdentityKey(peerJid, isMucPmPeer) !== dmConversationIdentityKey(failure.key, isMucPmPeer)
    ) return;
    refetchTimelineToCloseGap();
  }

  /** Wholesale MAM refetch of the active conversation. Local optimistic
   *  sends are preserved across the reload so the user keeps retry
   *  affordances. */
  function refetchTimelineToCloseGap() {
    const peerJid = activePeerJid.value;
    if (!peerJid) return;
    if (messages.value.length === 0) return;
    const snapshot = messages.value;
    const preserved = snapshot.filter(
      (m) =>
        m.isSelf && (
          m.deliveryStatus === "queued"
          || m.deliveryStatus === "sending"
          || m.deliveryStatus === "failed"
        ),
    );
    void (async () => {
      const result = await loadMessages(peerJid);
      // "aborted": a newer request (same or switched conversation) owns
      // the timeline now — reacting would push stale rows onto it.
      if (result === "aborted") return;
      if (activePeerJid.value !== peerJid) return;
      if (result === "failed") {
        // A failed reload must not leave the timeline wiped to
        // queued-only: the catch-up coverage skip would then block the
        // self-heal on the next reconnect. Restore the pre-reload
        // timeline (the load error stays surfaced) — merging in what
        // the catch kept (queued rows plus any live arrival during the
        // reload, #675), so neither vanishes until the next rebuild.
        const queuedDuringReload = messages.value.filter((m) => !findMessageById(snapshot, m.id));
        messages.value = [...snapshot, ...queuedDuringReload];
        return;
      }
      if (preserved.length === 0) return;
      const toAppend = preserved.filter((m) => !findMessageById(messages.value, m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }



  function clearMessages() {
    paging.reset();
    messageSearch.reset();
    pinnedEdgeScroller.cancelSettleLock();
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

  function applyMdsDisplayed(chatId: string, displayed: MdsDisplayedState): boolean {
    const peerJid = activePeerJid.value;
    const client = xmppClient.value;
    if (!peerJid || !client) return false;
    // Inbound XEP-0490 item IDs have already crossed the typed Rust
    // boundary: a resource-bearing id is an occupant identity and must
    // compare exactly even before custom-service discovery completes.
    const chatIdentity = resourceOf(chatId)
      ? fullJidIdentityKey(chatId)
      : bareJidKey(chatId);
    const explicitScope = activePeerJid.value === peerJid
      ? activeConversationScope?.value
      : null;
    const peerIdentity = dmConversationIdentityKey(peerJid, () =>
      explicitScope
        ? explicitScope === "muc-occupant"
        : resourceOf(chatId) !== "" || conversationScope(peerJid) === "muc-occupant"
    );
    if (chatIdentity !== peerIdentity) return false;
    const feedTimeline = messages.value.filter(isFeedVisible);
    const key = mdsChatKey(chatIdentity);
    if (!displayedStateCanAdvance(feedTimeline, getMdsDisplayed(key), displayed)) return false;
    firstUnseenId.value = firstUnseenIdAfterDisplayedState(
      feedTimeline,
      firstUnseenId.value,
      displayed,
    );
    return true;
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
    applyReaction(event.messageId, peerNameFromJid(event.reactorJid), event.emojis, event.occurredAt);
  }

  watch(scrollDirection, () => {
    void alignTimelineToPreference();
  });

  // Browser-tab refocus: re-pin if the user was already at the edge before
  // switching away. New messages that arrived while hidden didn't get a
  // chance to settle (rAF/RO throttled, virtualizer didn't measure
  // offscreen rows), so the position drifts away from the latest message.
  const { isWindowFocused } = useChatWindowVisibility();
  watch(isWindowFocused, (focused, prev) => {
    if (focused && !prev && pinnedEdgeScroller.isPinnedAtEdge.value) {
      void scrollToPinnedEdgeAndPin();
    }
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
    loadingOlderThreadIds,
    threadHasOlder,
    backfillThread,
    loadOlderThreadMessages,
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
    applyMdsDisplayed,
    disconnect,
    onIncomingMessage,
    onChatState,
    onDisplayed,
    onReaction,
    onMessageQueueStatus,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
    onCatchupFailed,
    scrollToPinnedEdge,
    isPinnedAtEdge: pinnedEdgeScroller.isPinnedAtEdge,
    latestRemoteMessageId,
  };
}
