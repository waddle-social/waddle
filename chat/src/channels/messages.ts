import { ref, computed, nextTick, watch, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import type { ChannelSummary } from "@/lib/chat-types";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  bareJidKey,
  barePeerJid,
  type CatchupConversationFailure,
  type LiveRoomMessage,
  type RoomActivityEvent,
  type RoomAccessChangedEvent,
  type SessionLifecycleEvent,
  type RoomAuthority,
  type RoomHats,
  type RoomPresence,
} from "@/lib/xmpp-client";
import { $xmppStatus } from "@/stores/xmpp-status";
import { applyPinEvent } from "@/stores/pinned-messages";
import { hydrateSinglePinnedBody } from "@/services/pinned-message-bodies";
import { trustedLinkPreviewMediaOrigin } from "@/lib/xmpp/link-preview";
import { roomMessageFromArchived } from "@/lib/xmpp/wasm-message-codecs";
import {
  type TimelineMessage,
} from "@/lib/chat-ui";
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
  listQueuedRoomMessages,
} from "@/lib/outbound-queue-store";
import { mentionMatchesBareJid } from "@/lib/mentions";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import { roomJidForChannelSummary } from "@/lib/channel-room";
import { withSpan } from "@/lib/telemetry";
import {
  applyForumContext,
  isFeedTimelineMessage,
  mapLiveRoomMessageToTimeline,
} from "@/channels/timeline";
import {
  queuedRoomMessageToTimeline,
} from "@/channels/message-timeline-state";
import { useChannelMamPaging } from "@/channels/mam-paging";
import { useMucSend } from "@/channels/muc-send";
import { useChannelMessageActions } from "@/channels/message-actions";
import { useChannelLiveMerge } from "@/channels/live-merge";
import { useChannelChatStates } from "@/channels/chat-states";
import { useChannelReadMarkers } from "@/channels/read-markers";
import { useChannelMessageSearch } from "@/channels/message-search";
import { useChatWindowVisibility } from "@/shell/window-visibility";

export function useChannelMessages(
  session: Ref<WaddleSession | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activeSpaceId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  currentChannel: Ref<ChannelSummary | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
  mentionJidsByNick?: Ref<Record<string, string>>,
  isChannelTimelineActive: Ref<boolean> = computed(() => true),
) {
  const { mode: scrollDirection } = useScrollDirectionPreference();
  // xmppStatus is owned by $xmppStatus (written from XmppProvider, which is
  // persisted across route changes). Reading via useStore keeps the composable
  // in sync with the authoritative snapshot on every mount.
  const xmppStatus = useStore($xmppStatus);

  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const forumPostTitle = ref("");
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const timelineEdgeScroller: Ref<((mode: ScrollDirectionMode) => boolean | Promise<boolean>) | null> = ref(null);
  const pinnedEdgeScroller = createPinnedEdgeScroller({
    element: timelineEl,
    mode: scrollDirection,
    virtualScroll: timelineEdgeScroller,
  });
  const roomHats = ref<RoomHats>({});
  const roomAuthority = ref<RoomAuthority>({});
  const roomPresence = ref<RoomPresence>({});
  const roomLastSeen = ref<Record<string, number>>({});
  const slowModeCooldown = ref(0);
  const activeChannels = ref<Set<string>>(new Set());
  const mentionedChannelCounts = ref<Record<string, number>>({});
  const lastMentionActivity = ref<RoomActivityEvent | null>(null);
  // Stanza-ids whose mention has been counted, insertion-ordered for FIFO
  // eviction. Bounds memory across long sessions; the cap only needs to
  // outlast the window in which a live copy and its MAM catch-up
  // re-emission can both arrive.
  const recordedMentionStanzaIds = new Set<string>();
  const RECORDED_MENTION_STANZA_IDS_CAP = 512;
  const pendingNotificationActivities = ref<RoomActivityEvent[]>([]);
  const roomAvatarHashes = ref<Record<string, string>>({});
  const roomJidOverrides = ref<Record<string, string>>({});

  const chatStates = useChannelChatStates({
    xmppClient,
    activeSpaceId,
    activeChannelId,
  });
  const {
    typingUsers,
    addTypingUser,
    removeTypingUser,
    clearTypingState,
    notifyComposing,
  } = chatStates;

  const readMarkers = useChannelReadMarkers({
    xmppClient,
    activeSpaceId,
    activeChannelId,
    currentChannel,
    messages,
  });
  const {
    firstUnseenId,
    latestRemoteMessageId,
    markDisplayed,
    persistLastSeen,
  } = readMarkers;

  const messageSearch = useChannelMessageSearch({
    xmppClient,
    activeSpaceId,
    activeChannelId,
    actionError,
    clearActionError,
    normalizeError,
  });
  const {
    searchQuery,
    searchResults,
    isSearching,
    searchMessages,
    clearSearch,
  } = messageSearch;

  const currentRoomJid = computed(() => {
    if (!session.value || !activeChannelId.value) return null;
    return roomJidForChannel(activeChannelId.value);
  });
  const activeTimelineRoomJid = computed(() =>
    isChannelTimelineActive.value ? currentRoomJid.value : null
  );
  type RequiredRoomAccess = Pick<
    Extract<RoomAccessChangedEvent, { state: "required" }>,
    "roomJid" | "condition"
  >;
  const roomAccessRequirements = ref<Record<string, RequiredRoomAccess>>({});
  const currentRoomAccessRequirement = computed(() => {
    const roomJid = currentRoomJid.value;
    return roomJid ? roomAccessRequirements.value[bareJidKey(roomJid)] ?? null : null;
  });

  function applyRoomAccessEvent(event: RoomAccessChangedEvent) {
    const key = bareJidKey(event.roomJid);
    if (event.state === "required") {
      roomAccessRequirements.value = {
        ...roomAccessRequirements.value,
        [key]: {
          roomJid: event.roomJid,
          condition: event.condition,
        },
      };
      return;
    }
    const next = { ...roomAccessRequirements.value };
    delete next[key];
    roomAccessRequirements.value = next;
  }

  function roomJidForChannel(channelId: string): string | null {
    const currentSession = session.value;
    if (!currentSession) return null;
    const channel = currentChannel.value?.id === channelId ? currentChannel.value : null;
    if (channel?.jid) return barePeerJid(channel.jid);
    const override = roomJidOverrides.value[channelId];
    if (override) return override;
    return roomJidForChannelSummary(currentSession, channel ?? { id: channelId });
  }

  function rememberChannelRoomJid(channelId: string, roomJid: string) {
    const normalizedRoomJid = barePeerJid(roomJid);
    if (!channelId || !normalizedRoomJid) return;
    roomJidOverrides.value = {
      ...roomJidOverrides.value,
      [channelId]: normalizedRoomJid,
    };
  }

  function queuedMessagesForRoom(roomJid: string): TimelineMessage[] {
    const currentSession = session.value;
    if (!currentSession) return [];
    const queued = listQueuedRoomMessages(barePeerJid(currentSession.jid), roomJid);
    for (const message of queued) pendingEchoClientIds.add(message.id);
    return queued.map((message) => queuedRoomMessageToTimeline(currentSession, roomJid, message));
  }

  function appendQueuedMessages(timeline: TimelineMessage[], roomJid: string): TimelineMessage[] {
    return mergeQueuedIntoTimeline(timeline, queuedMessagesForRoom(roomJid), applyForumContext);
  }

  watch(xmppClient, (client, _previousClient, onCleanup) => {
    if (client) {
      client.setMessageHandler((msg) => {
        // Out-of-room body-bearing message: record cross-room activity
        // (channel-list unread badge, etc.) and do not route to the
        // in-room live-merge path. Retractions / corrections targeting
        // other rooms are silently ignored.
        if (
          (!activeTimelineRoomJid.value || msg.roomJid !== activeTimelineRoomJid.value) &&
          msg.type === "message" &&
          msg.body &&
          !msg.replacesId &&
          !msg.retractsId
        ) {
          recordRoomActivity({
            roomJid: msg.roomJid,
            nick: msg.nick,
            body: msg.body,
            ...(msg.stanzaId ? { stanzaId: msg.stanzaId } : {}),
            ...(msg.mentions ? { mentions: msg.mentions } : {}),
            ...(msg.broadcastMention ? { broadcastMention: msg.broadcastMention } : {}),
            ...(msg.createdAtSource === "archive" ? { fromArchive: true } : {}),
          });
          return;
        }
        if (
          !activeTimelineRoomJid.value ||
          msg.roomJid !== activeTimelineRoomJid.value ||
          msg.type !== "message"
        )
          return;
        // When a user sends a real message, clear their typing state
        removeTypingUser(msg.nick);

        // Typed dispatch via the live-merge composable. The classifier
        // inside handleRoomMessage routes to applyRetraction /
        // applyCorrection / mergeLiveMessage based on the stanza shape.
        const classified = liveMerge.handleRoomMessage(msg);

        // Foreground notification routing stays in the orchestrator —
        // it depends on cross-room session state and the tab-visibility
        // signal, both of which are out of scope for live-merge.
        if (classified.kind !== "live") return;
        const isMentioned =
          !!msg.broadcastMention ||
          msg.mentions?.some(isSessionMention);
        if (msg.nick !== session.value?.username && isTabHidden()) {
          const activity: RoomActivityEvent = {
            roomJid: msg.roomJid,
            nick: msg.nick,
            body: msg.body,
          };
          if (msg.stanzaId) activity.stanzaId = msg.stanzaId;
          if (msg.mentions) activity.mentions = msg.mentions;
          if (msg.broadcastMention) activity.broadcastMention = msg.broadcastMention;
          if (msg.createdAtSource === "archive") activity.fromArchive = true;
          // Same policy as `recordRoomActivity`: archive decodes (MAM
          // catch-up re-emissions) never notify, but a genuinely-missed
          // mention still records — idempotent by stanza-id.
          if (!activity.fromArchive) enqueueNotificationActivity(activity);
          if (isMentioned && shouldRecordMentionOnce(activity)) {
            recordMentionActivity(activity);
            lastMentionActivity.value = activity;
          }
        } else if (isMentioned && msg.stanzaId && isFeedVisibleRoomMessage(msg)) {
          // Rendered in the open channel with the tab visible: the
          // mention is SEEN, not missed. Account it without recording so
          // a later re-emission of the same stanza (catch-up page, or
          // the activity route after a room switch) cannot badge a
          // message the user already read. Thread replies are excluded —
          // the feed hides them (mirroring the `isFeedVisible` read-marker
          // policy), so on-screen never meant seen for those.
          accountMentionStanzaId(msg.stanzaId);
        }
      });
      client.setChatStateHandler((event) => {
        if (!activeTimelineRoomJid.value || event.roomJid !== activeTimelineRoomJid.value) return;
        if (event.state === "composing") {
          addTypingUser(event.nick);
        } else {
          removeTypingUser(event.nick);
        }
      });
      client.setReactionHandler((event) => {
        if (!activeTimelineRoomJid.value || event.roomJid !== activeTimelineRoomJid.value) return;
        applyReaction(
          event.messageId,
          event.nick,
          event.emojis,
          event.authorRealJid ?? `${event.roomJid}/${event.nick}`,
          event.occurredAt,
        );
      });
      // #414: route pin/unpin system events into the pinned-messages
      // store so PinnedPanel + the badge in MessageCard update live.
      // Tests use a stub client that may not implement this; guard.
      client.setPinEventHandler?.(({ roomJid, event }) => {
        if (event.action === "pinned" && event.preview) {
          applyPinEvent(roomJid, {
            action: "pinned",
            target_stanza_id: event.target_stanza_id,
            entry: {
              target_stanza_id: event.target_stanza_id,
              pinner_jid: event.by,
              pinned_at: new Date().toISOString(),
              preview: event.preview,
            },
          });
          // Waddle MAM stanza-id filter: fetch the full message body so
          // PinnedPanel can render the message without a panel re-open.
          // Skip if the client does not support the stanza-id MAM filter.
          if (
            activeTimelineRoomJid.value === roomJid &&
            xmppClient.value &&
            "fetchRoomMessagesByStanzaIds" in xmppClient.value
          ) {
            const currentClient = xmppClient.value;
            const spaceId = activeSpaceId.value ?? "";
            const channelId = activeChannelId.value ?? "";
            const convertForTimeline = (a: Parameters<typeof roomMessageFromArchived>[0]) => {
              const live = roomMessageFromArchived(a, {
                trustedMediaOrigin: session.value
                  ? trustedLinkPreviewMediaOrigin(session.value)
                  : null,
              });
              return live && session.value
                ? mapLiveRoomMessageToTimeline(session.value, live)
                : null;
            };
            void hydrateSinglePinnedBody({
              fetchByStanzaIds: (stanzaIds) =>
                currentClient.fetchRoomMessagesByStanzaIds(spaceId, channelId, stanzaIds),
              spaceId,
              channelId,
              roomJid,
              stanzaId: event.target_stanza_id,
              timelineMessages: messages.value,
              convert: convertForTimeline,
            }).catch((error) => console.warn("hydrateSinglePinnedBody failed", error));
          }
        } else {
          applyPinEvent(roomJid, {
            action: event.action,
            target_stanza_id: event.target_stanza_id,
          });
        }
      });
      client.setDisplayedHandler((event) => {
        if (!activeTimelineRoomJid.value || event.roomJid !== activeTimelineRoomJid.value) return;
        applyDisplayed(event.messageId, event.nick);
      });
      client.setHatsHandler((hats) => {
        roomHats.value = hats;
      });
      client.setAuthorityHandler((authority) => {
        roomAuthority.value = authority;
      });
      client.setPresenceHandler((presence) => {
        roomPresence.value = presence;
      });
      client.setLastSeenHandler((nick, timestamp) => {
        roomLastSeen.value = { ...roomLastSeen.value, [nick]: timestamp };
      });
      client.setActivityHandler((event) => {
        recordRoomActivity(event);
      });
      // XEP-0486: Track room avatar hashes from presence
      client.setRoomAvatarHandler((roomJid, hash) => {
        roomAvatarHashes.value = { ...roomAvatarHashes.value, [roomJid]: hash };
      });
      const unsubscribeRoomAccess = client.onRoomAccessChanged?.(applyRoomAccessEvent) ?? (() => {});
      onCleanup(unsubscribeRoomAccess);
      roomAccessRequirements.value = {};
      for (const requirement of client.listRoomAccessRequirements?.() ?? []) {
        applyRoomAccessEvent(requirement);
      }
    } else {
      // $xmppStatus is reset by XmppProvider on logout/unmount.
      clearTypingState();
      clearLiveActivityState();
      roomAccessRequirements.value = {};
    }
  }, { immediate: true });

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
    // Prefer scrolling to the "New messages" divider (rendered immediately
    // above the first-unseen MessageCard). Aligning the message itself to
    // `block: "start"` would push the divider off-screen.
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

  /**
   * On a fresh XMPP session (resume failed or first connect after a drop),
   * refetch MAM to close any message gap for the current channel — unless
   * the client's own reconnect catch-up covers this room (#1180): a
   * wholesale reload would then be a second concurrent MAM fetch racing
   * the catch-up's merges for messages.value. The catch-up path (cursor
   * paging + live-merge) fully closes the gap, same choreography as a
   * resumed session. Resumed sessions never call this — the server
   * replays everything.
   */
  function onSessionLifecycle(event: SessionLifecycleEvent) {
    if (event.type !== "fresh") return;
    const channelId = activeChannelId.value;
    if (!channelId) return;
    // Coverage keys are server-emitted (RFC 7622-lowercased) JIDs while
    // the directory-derived room JID may differ in case — compare via
    // bareJidKey or the skip silently fails open back into the race.
    const roomJid = roomJidForChannel(channelId);
    if (roomJid && event.catchup.roomJids.some((jid) => bareJidKey(jid) === bareJidKey(roomJid))) return;
    refetchTimelineToCloseGap();
  }

  /**
   * Fallback for a covered-but-failed reconnect catch-up (#1180): the
   * lifecycle skip above trusted the catch-up to close this room's gap;
   * when it fails, run the wholesale reload it suppressed — serialized
   * after the failed attempt, so the two fetches can no longer race.
   */
  function onCatchupFailed(failure: CatchupConversationFailure) {
    if (failure.kind !== "room") return;
    const channelId = activeChannelId.value;
    if (!channelId) return;
    const roomJid = roomJidForChannel(channelId);
    if (!roomJid || bareJidKey(roomJid) !== bareJidKey(failure.key)) return;
    refetchTimelineToCloseGap();
  }

  /**
   * Wholesale MAM refetch of the active channel. Local optimistic sends
   * (queued/sending/failed) are preserved across the reload so the UI
   * doesn't drop unsent entries the user can still retry.
   */
  function refetchTimelineToCloseGap() {
    const spaceId = activeSpaceId.value;
    const channelId = activeChannelId.value;
    if (!channelId) return;
    // Only catch up if we had already loaded this channel; otherwise the
    // standard loadMessages call on channel-select handles it.
    if (messages.value.length === 0) return;
    const metadataSeed = messages.value;
    const preserved = messages.value.filter(
      (m) =>
        m.isSelf && (
          m.deliveryStatus === "queued"
          || m.deliveryStatus === "sending"
          || m.deliveryStatus === "failed"
        ),
    );
    void (async () => {
      const result = await loadMessages(spaceId ?? "", channelId, 0, metadataSeed);
      // "aborted": a newer request (same or switched conversation) owns
      // the timeline now — reacting would push stale rows onto it.
      if (result === "aborted") return;
      if (
        (activeSpaceId.value ?? "") !== (spaceId ?? "") ||
        activeChannelId.value !== channelId
      )
        return;
      if (result === "failed") {
        // A failed reload must not leave the timeline wiped to
        // queued-only: the catch-up coverage skip would then block the
        // self-heal on the next reconnect. Restore the pre-reload
        // timeline (the load error stays surfaced) — merging in what
        // the catch kept (queued rows plus any live arrival during the
        // reload, #675), so neither vanishes until the next rebuild.
        const queuedDuringReload = messages.value.filter((m) => !findMessageById(metadataSeed, m.id));
        messages.value = [...metadataSeed, ...queuedDuringReload];
        return;
      }
      if (preserved.length === 0) return;
      const toAppend = preserved.filter((m) => !findMessageById(messages.value, m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }

  // Matches ContentArea.feedMessages: thread replies (messages with a
  // threadId that isn't their own id) are hidden, so the last-seen anchor
  // and the "New messages" divider must be computed against this predicate.
  const isFeedVisible = isFeedTimelineMessage;

  const send = useMucSend({
    session,
    xmppClient,
    activeSpaceId,
    activeChannelId,
    currentChannel,
    currentRoomJid,
    messages,
    draft,
    forumPostTitle,
    actionError,
    clearActionError,
    normalizeError,
    ...(mentionJidsByNick ? { mentionJidsByNick } : {}),
    scrollToPinnedEdgeAndPin,
    // Post-send XEP-0085 cleanup. The composing-timer + lastChatState
    // bookkeeping moved into useChannelChatStates in PR 5; the
    // orchestrator routes the post-send signal via `resetOnSend()` and
    // emits the active state through the client.
    onSendComplete: (result, spaceId, channelId, isStillCurrentChannel) => {
      if (!isStillCurrentChannel) {
        // Client swap or channel change happened during the send. Don't
        // emit XEP-0085 "active" through the new session targeting the
        // old room — and skip the composing-timer cleanup since it
        // belonged to the prior session.
        return;
      }
      chatStates.resetOnSend();
      if (result?.state === "sending" && xmppClient.value) {
        void xmppClient.value.sendChatState(spaceId, channelId, "active").catch(() => undefined);
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

  const liveMerge = useChannelLiveMerge({
    session,
    messages,
    activeChannelId,
    pendingEchoClientIds,
    scrollToPinnedEdgeAndPin,
    persistLastSeen,
  });
  const { applyDisplayed, applyReaction } = liveMerge;

  const actions = useChannelMessageActions({
    session,
    xmppClient,
    activeSpaceId,
    activeChannelId,
    currentRoomJid,
    messages,
    actionError,
    clearActionError,
    normalizeError,
    applyReaction,
  });
  const {
    toggleReaction,
    retractMessage,
    moderateMessage,
    invokeExtensionAction,
  } = actions;

  const paging = useChannelMamPaging({
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

  // Channel-load entry point. The MAM-paging composable owns paging state,
  // but search state lives here, so we reset it before delegating — keeping
  // the pre-#188 invariant that switching channels invalidates in-flight
  // searches and clears stale results.
  async function loadMessages(
    spaceId: string,
    channelId: string,
    unreadAtLoad = 0,
    metadataSeed: TimelineMessage[] = [],
  ) {
    messageSearch.reset();
    return withSpan(
      "xmpp.initial_render",
      { "conversation.kind": "room" },
      () => paging.loadMessages(spaceId, channelId, unreadAtLoad, metadataSeed),
    );
  }

  async function selectChannel(channelId: string) {
    messages.value = [];
    clearTypingState();
    await loadMessages(activeSpaceId.value ?? "", channelId);
  }

  function disconnect() {
    paging.reset();
    messageSearch.reset();
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    roomJidOverrides.value = {};
    // $xmppStatus is authoritative and owned by XmppProvider; do not write it here.
    clearTypingState();
    clearLiveActivityState();
    firstUnseenId.value = null;
  }

  function clearMessages() {
    paging.reset();
    messageSearch.reset();
    pinnedEdgeScroller.cancelSettleLock();
    pendingEchoClientIds.clear();
    messages.value = [];
    clearTypingState();
    firstUnseenId.value = null;
  }

  function applyMdsDisplayed(chatId: string, displayed: MdsDisplayedState): boolean {
    const roomJid = currentRoomJid.value;
    // XEP-0490 scopes groupchat to the bare room and MUC PMs to a full
    // occupant JID. Never let an occupant item advance the room timeline.
    if (!roomJid || chatId.includes("/") || barePeerJid(chatId) !== barePeerJid(roomJid)) {
      return false;
    }
    const feedTimeline = messages.value.filter(isFeedVisible);
    const key = mdsChatKey(barePeerJid(roomJid));
    if (!displayedStateCanAdvance(feedTimeline, getMdsDisplayed(key), displayed)) return false;
    firstUnseenId.value = firstUnseenIdAfterDisplayedState(
      feedTimeline,
      firstUnseenId.value,
      displayed,
    );
    return true;
  }

  function clearChannelActivity(roomJid: string) {
    const bareRoomJid = barePeerJid(roomJid);
    const next = new Set(activeChannels.value);
    next.delete(bareRoomJid);
    activeChannels.value = next;
    if (mentionedChannelCounts.value[bareRoomJid]) {
      const remaining = { ...mentionedChannelCounts.value };
      delete remaining[bareRoomJid];
      mentionedChannelCounts.value = remaining;
    }
  }

  function clearLiveActivityState() {
    activeChannels.value = new Set();
    mentionedChannelCounts.value = {};
    lastMentionActivity.value = null;
    pendingNotificationActivities.value = [];
    recordedMentionStanzaIds.clear();
  }

  function isOwnMentionActivity(event: RoomActivityEvent): boolean {
    if (event.nick === session.value?.username) return false;
    return !!event.broadcastMention
      || !!event.mentions?.some(isSessionMention);
  }

  function isSessionMention(mention: string): boolean {
    return mentionMatchesBareJid(mention, session.value?.jid);
  }

  function recordMentionActivity(event: RoomActivityEvent) {
    const roomJid = barePeerJid(event.roomJid);
    mentionedChannelCounts.value = {
      ...mentionedChannelCounts.value,
      [roomJid]: (mentionedChannelCounts.value[roomJid] ?? 0) + 1,
    };
  }

  function recordRoomActivity(event: RoomActivityEvent) {
    const roomJid = barePeerJid(event.roomJid);
    activeChannels.value = new Set([...activeChannels.value, roomJid]);
    // MAM catch-up re-emissions never notify: a replayed banner for a
    // message the user may already have seen is spam, and the push
    // pipeline covers the disconnected window.
    if (!event.fromArchive && event.nick !== session.value?.username && isTabHidden()) {
      enqueueNotificationActivity(event);
    }
    // Mentions DO record from catch-up — the server inbox tracks unread
    // only, not mentions, so a mention missed while disconnected has no
    // other path to the badge. Idempotent by XEP-0359 stanza-id (identical
    // on the live and archive copies of a message), so a catch-up
    // re-emission of an already-recorded mention cannot inflate the count.
    if (isOwnMentionActivity(event) && shouldRecordMentionOnce(event)) {
      recordMentionActivity(event);
      lastMentionActivity.value = event;
    }
  }

  /** `isFeedTimelineMessage` for the raw live shape: thread replies are
   * hidden from the feed, so being in the open channel never showed them. */
  function isFeedVisibleRoomMessage(msg: LiveRoomMessage): boolean {
    return !msg.threadId || msg.id === msg.threadId || !!msg.callThread;
  }

  /**
   * Marks a mention's stanza-id as accounted (badge recorded OR read on
   * screen). Returns false when it was already accounted, so a MAM
   * catch-up re-emission of the same message can never badge twice.
   */
  function accountMentionStanzaId(stanzaId: string): boolean {
    if (recordedMentionStanzaIds.has(stanzaId)) return false;
    recordedMentionStanzaIds.add(stanzaId);
    if (recordedMentionStanzaIds.size > RECORDED_MENTION_STANZA_IDS_CAP) {
      const oldest = recordedMentionStanzaIds.values().next().value;
      if (oldest !== undefined) recordedMentionStanzaIds.delete(oldest);
    }
    return true;
  }

  /**
   * Stanza-id-keyed idempotency for mention accounting. Events without a
   * stanza-id cannot be deduplicated: live ones record (a conformant
   * server stamps every archived message, so an unstamped stanza has no
   * archive copy to collide with), archive ones skip (their live twin may
   * already have been seen, and a missed count is recoverable while an
   * inflated one is not).
   */
  function shouldRecordMentionOnce(event: RoomActivityEvent): boolean {
    if (!event.stanzaId) return !event.fromArchive;
    return accountMentionStanzaId(event.stanzaId);
  }

  function isTabHidden(): boolean {
    return typeof document !== "undefined"
      && (!document.hasFocus() || document.visibilityState === "hidden");
  }

  function enqueueNotificationActivity(event: RoomActivityEvent) {
    pendingNotificationActivities.value = [...pendingNotificationActivities.value, event];
  }

  watch(scrollDirection, () => {
    void alignTimelineToPreference();
  });

  // Browser-tab refocus: re-pin if the user was already at the edge before
  // switching away. Mirrors the DM-side watcher; see `dms/messages.ts`.
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
      const spaceId = activeSpaceId.value ?? "";
      const channelId = activeChannelId.value;
      const pinned = await scrollToPinnedEdgeAndPin();
      if (
        pinned &&
        requestId === paging.currentRequestId() &&
        (activeSpaceId.value ?? "") === spaceId &&
        activeChannelId.value === channelId &&
        messages.value.some(isFeedVisible)
      ) {
        paging.markInitialLatestPagePinned();
        const newest = [...messages.value].reverse().find(isFeedVisible);
        if (channelId && newest) persistLastSeen(channelId, newest.id);
      }
    },
    { flush: "post" },
  );

  return {
    xmppStatus,
    messages,
    firstUnseenId,
    draft,
    forumPostTitle,
    isLoadingMessages,
    isLoadingOlderMessages,
    hasOlderMessages,
    isSending,
    timelineEl,
    timelineEdgeScroller,
    currentRoomJid,
    currentRoomAccessRequirement,
    typingUsers,
    roomHats,
    roomAuthority,
    roomPresence,
    roomLastSeen,
    slowModeCooldown,
    loadMessages,
    rememberChannelRoomJid,
    loadOlderMessages,
    ensureMessageLoaded,
    backfillThread,
    loadOlderThreadMessages,
    loadingOlderThreadIds,
    threadHasOlder,
    selectChannel,
    sendMessage,
    uploadProgress,
    editMessage,
    retractMessage,
    moderateMessage,
    invokeExtensionAction,
    toggleReaction,
    markDisplayed,
    notifyComposing,
    applyMdsDisplayed,
    disconnect,
    clearMessages,
    activeChannels,
    mentionedChannelCounts,
    roomAvatarHashes,
    searchQuery,
    searchResults,
    isSearching,
    searchMessages,
    clearSearch,
    clearChannelActivity,
    scrollToPinnedEdge,
    isPinnedAtEdge: pinnedEdgeScroller.isPinnedAtEdge,
    latestRemoteMessageId,
    lastMentionActivity,
    pendingNotificationActivities,
    onMessageQueueStatus,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
    onCatchupFailed,
  };
}
