import { ref, computed, nextTick, watch, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import type { ChannelSummary } from "@/lib/chat-types";
import { isForumChannel } from "@/lib/channel-types";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  barePeerJid,
  type LiveRoomMessage,
  type MessageSearchResult,
  type RoomActivityEvent,
  type SessionLifecycleEvent,
  type ChatStateType,
  type RoomHats,
  type RoomPresence,
} from "@/lib/xmpp-client";
import { $xmppStatus } from "@/stores/xmpp-status";
import {
  inferredFileDisposition,
  type DeliveryStatus,
  type ExtensionAnnotationAction,
  type MarkupSpan,
  type MessageReference,
  type TimelineMessage,
} from "@/lib/chat-ui";
import { MAX_FILE_UPLOAD_BYTES } from "@/lib/xmpp/file-upload";
import type { OutboundFileAttachment } from "@/lib/xmpp";
import {
  findMessageById,
  indexMessageByIds,
  matchMessageId,
  mergeMessageIds,
} from "@/lib/message-ids";
import { findMessageElementById } from "@/lib/message-targeting";
import { isTopPinnedScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import { createPinnedEdgeScroller } from "@/lib/pinned-edge-scroll";
import { roomKey, setLastSeen } from "@/lib/last-seen-store";
import {
  listQueuedRoomMessages,
  type PersistedQueuedRoomMessage,
} from "@/lib/outbound-queue-store";
import { mentionMatchesUsername } from "@/lib/mentions";
import { useScrollDirectionPreference } from "@/preferences/scroll-direction";
import { roomJidForChannelSummary } from "@/lib/channel-room";
import {
  applyForumContext,
  isFeedTimelineMessage,
  mapLiveRoomMessageToTimeline,
} from "@/channels/timeline";

function mergeReplyToMetadata(
  existing: TimelineMessage["replyTo"],
  incoming: TimelineMessage["replyTo"],
): TimelineMessage["replyTo"] {
  if (!incoming) return existing;
  if (!existing) return { ...incoming };
  if (existing.id !== incoming.id) return existing;

  let next = existing;
  if (!next.author && incoming.author) next = { ...next, author: incoming.author };
  if (!next.preview && incoming.preview) next = { ...next, preview: incoming.preview };
  return next;
}

function sameStringList(a: readonly string[] | undefined, b: readonly string[] | undefined): boolean {
  const left = a ?? [];
  const right = b ?? [];
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function mergeMissingThreadMetadata(
  existing: TimelineMessage,
  incoming: TimelineMessage,
): TimelineMessage {
  let next = existing;
  const assign = (patch: Partial<TimelineMessage>) => {
    next = next === existing ? { ...existing, ...patch } : { ...next, ...patch };
  };

  const ids = mergeMessageIds(next, next.id, [incoming.id, ...(incoming.wireIds ?? [])]);
  if (ids.id !== next.id || !sameStringList(ids.wireIds, next.wireIds)) next = ids;

  if (!next.threadId && incoming.threadId) assign({ threadId: incoming.threadId });
  if (!next.parentThreadId && incoming.parentThreadId) assign({ parentThreadId: incoming.parentThreadId });
  if (!next.correctionTargetId && incoming.correctionTargetId) {
    assign({ correctionTargetId: incoming.correctionTargetId });
  }
  if (!next.reactionTargetId && incoming.reactionTargetId) {
    assign({ reactionTargetId: incoming.reactionTargetId });
  }

  const replyTo = mergeReplyToMetadata(next.replyTo, incoming.replyTo);
  if (replyTo !== next.replyTo) assign({ replyTo });

  return next;
}

interface TimelineBuildOptions {
  seedExistingOnly?: boolean;
}

function queuedRoomMessageToTimeline(
  session: WaddleSession,
  roomJid: string,
  queued: PersistedQueuedRoomMessage,
): TimelineMessage {
  const message: TimelineMessage = {
    id: queued.id,
    correctionTargetId: queued.id,
    author: session.username,
    authorJid: `${roomJid}/${session.username}`,
    body: queued.body || (queued.files?.[0]?.url ?? ""),
    createdAt: queued.createdAt,
    isSelf: true,
    deliveryStatus: "queued",
  };
  if (queued.markup && queued.markup.length > 0) message.markup = queued.markup;
  if (queued.references && queued.references.length > 0) message.references = queued.references;
  if (queued.replyTo) {
    message.replyTo = {
      id: queued.replyTo.id,
      ...(queued.replyTo.author ? { author: queued.replyTo.author } : {}),
      ...(queued.replyTo.body ? { preview: queued.replyTo.body } : {}),
    };
  }
  if (queued.threadId) message.threadId = queued.threadId;
  if (queued.parentThreadId) message.parentThreadId = queued.parentThreadId;
  if (queued.threadCreate) {
    message.threadId = queued.id;
    message.forumPostKind = "topic";
    message.forumTitle = queued.threadCreate.title;
    message.forumThreadTitle = queued.threadCreate.title;
  } else if (queued.threadReply) {
    message.forumPostKind = "reply";
  }
  if (queued.files && queued.files.length > 0) {
    message.sharedFiles = queued.files.map((file) => ({
      url: file.url,
      name: file.name,
      mediaType: file.mediaType,
      size: file.size,
      ...(file.width ? { width: file.width } : {}),
      ...(file.height ? { height: file.height } : {}),
      disposition: inferredFileDisposition(file.mediaType, file.name ?? file.url),
      ...(file.encrypted ? { encrypted: file.encrypted } : {}),
    }));
  }
  return message;
}

export function useChannelMessages(
  session: Ref<WaddleSession | null>,
  _api: Ref<null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activeSpaceId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  currentChannel: Ref<ChannelSummary | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
  mentionJidsByNick?: Ref<Record<string, string>>,
) {
  const { mode: scrollDirection } = useScrollDirectionPreference();
  // xmppStatus is owned by $xmppStatus (written from XmppProvider, which is
  // persisted across route changes). Reading via useStore keeps the composable
  // in sync with the authoritative snapshot on every mount.
  const xmppStatus = useStore($xmppStatus);

  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const forumPostTitle = ref("");
  const isLoadingMessages = ref(false);
  const isLoadingOlderMessages = ref(false);
  const hasOlderMessages = ref(true);
  const loadingOlderThreadIds = ref<Set<string>>(new Set());
  const threadHasOlder = ref<Record<string, boolean>>({});
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const timelineEdgeScroller: Ref<((mode: ScrollDirectionMode) => boolean | Promise<boolean>) | null> = ref(null);
  const pinnedEdgeScroller = createPinnedEdgeScroller({
    element: timelineEl,
    mode: scrollDirection,
    virtualScroll: timelineEdgeScroller,
  });
  const typingUsers = ref<string[]>([]);
  const roomHats = ref<RoomHats>({});
  const roomPresence = ref<RoomPresence>({});
  const roomLastSeen = ref<Record<string, number>>({});
  const slowModeCooldown = ref(0);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });
  const firstUnseenId = ref<string | null>(null);
  const latestRemoteMessageId = computed<string | null>(() => {
    for (let i = messages.value.length - 1; i >= 0; i--) {
      const m = messages.value[i];
      if (!m || m.isSelf || m.isRetracted) continue;
      return m.id;
    }
    return null;
  });
  const activeChannels = ref<Set<string>>(new Set());
  const lastMentionActivity = ref<RoomActivityEvent | null>(null);
  const roomAvatarHashes = ref<Record<string, string>>({});
  const searchQuery = ref("");
  const searchResults = ref<MessageSearchResult[]>([]);
  const isSearching = ref(false);
  let slowModeTimer: ReturnType<typeof setInterval> | null = null;

  let messageRequestId = 0;
  let oldestArchiveId: string | null = null;
  let initialLatestPagePinned = false;
  const oldestThreadArchiveIds = new Map<string, string>();
  let searchRequestId = 0;
  let lastChatState: ChatStateType = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  // Client-assigned stanza ids still awaiting MUC-echo reconciliation. A self
  // echo with a server-rewritten id falls back to body matching, but only
  // against messages in this set — otherwise sending the same text twice
  // would cause the second echo to rewrite the first (already reconciled)
  // message's id.
  const pendingEchoClientIds = new Set<string>();

  const currentRoomJid = computed(() => {
    if (!session.value || !activeChannelId.value) return null;
    return roomJidForChannel(activeChannelId.value);
  });
  const channelIsForum = computed(() => isForumChannel(currentChannel.value));

  function roomJidForChannel(channelId: string): string | null {
    const currentSession = session.value;
    if (!currentSession) return null;
    const channel = currentChannel.value?.id === channelId
      ? currentChannel.value
      : { id: channelId };
    return roomJidForChannelSummary(currentSession, channel);
  }

  function queuedMessagesForRoom(roomJid: string): TimelineMessage[] {
    const currentSession = session.value;
    if (!currentSession) return [];
    const queued = listQueuedRoomMessages(barePeerJid(currentSession.jid), roomJid);
    for (const message of queued) pendingEchoClientIds.add(message.id);
    return queued.map((message) => queuedRoomMessageToTimeline(currentSession, roomJid, message));
  }

  function appendQueuedMessages(timeline: TimelineMessage[], roomJid: string): TimelineMessage[] {
    const queued = queuedMessagesForRoom(roomJid).filter((message) => !findMessageById(timeline, message.id));
    return queued.length > 0 ? applyForumContext([...timeline, ...queued]) : timeline;
  }

  watch(xmppClient, (client) => {
    if (client) {
      client.setMessageHandler((msg) => {
        if (
          !currentRoomJid.value ||
          msg.roomJid !== currentRoomJid.value ||
          msg.type !== "message"
        )
          return;
        // When a user sends a real message, clear their typing state
        removeTypingUser(msg.nick);

        // XEP-0424: Handle message retractions
        if (msg.retractsId) {
          applyRetraction(msg.retractsId);
          return;
        }

        // XEP-0308: Handle message corrections
        if (msg.replacesId) {
          applyCorrection(
            msg.replacesId,
            msg.body,
            mucCorrectionSender(msg),
            msg.markup,
            msg.references,
            msg.extensionAnnotations,
          );
          return;
        }

        mergeLiveMessage(
          mapLiveRoomMessageToTimeline(session.value!, msg, (id) => findMessageById(messages.value, id)),
        );

        // Trigger notification for mentions when tab is unfocused
        const isTabHidden = typeof document !== "undefined"
          && (!document.hasFocus() || document.visibilityState === "hidden");
        if (msg.nick !== session.value?.username && isTabHidden) {
          const isMentioned =
            !!msg.broadcastMention ||
            msg.mentions?.some((mention) => mentionMatchesUsername(mention, session.value?.username));
          if (isMentioned) {
            const activity: RoomActivityEvent = {
              roomJid: msg.roomJid,
              nick: msg.nick,
              body: msg.body,
            };
            if (msg.mentions) activity.mentions = msg.mentions;
            if (msg.broadcastMention) activity.broadcastMention = msg.broadcastMention;
            lastMentionActivity.value = activity;
          }
        }
      });
      client.setChatStateHandler((event) => {
        if (!currentRoomJid.value || event.roomJid !== currentRoomJid.value) return;
        if (event.state === "composing") {
          addTypingUser(event.nick);
        } else {
          removeTypingUser(event.nick);
        }
      });
      client.setReactionHandler((event) => {
        if (!currentRoomJid.value || event.roomJid !== currentRoomJid.value) return;
        applyReaction(
          event.messageId,
          event.nick,
          event.emojis,
          event.authorRealJid ?? `${event.roomJid}/${event.nick}`,
        );
      });
      client.setDisplayedHandler((event) => {
        if (!currentRoomJid.value || event.roomJid !== currentRoomJid.value) return;
        applyDisplayed(event.messageId, event.nick);
      });
      client.setHatsHandler((hats) => {
        roomHats.value = hats;
      });
      client.setPresenceHandler((presence) => {
        roomPresence.value = presence;
      });
      client.setLastSeenHandler((nick, timestamp) => {
        roomLastSeen.value = { ...roomLastSeen.value, [nick]: timestamp };
      });
      client.setActivityHandler((event) => {
        activeChannels.value = new Set([...activeChannels.value, event.roomJid]);
        if (event.mentions?.length || event.broadcastMention) {
          lastMentionActivity.value = event;
        }
      });
      // XEP-0486: Track room avatar hashes from presence
      client.setRoomAvatarHandler((roomJid, hash) => {
        roomAvatarHashes.value = { ...roomAvatarHashes.value, [roomJid]: hash };
      });
      client.setSlowModeHandler((seconds) => {
        slowModeCooldown.value = seconds;
        if (slowModeTimer) clearInterval(slowModeTimer);
        slowModeTimer = setInterval(() => {
          slowModeCooldown.value--;
          if (slowModeCooldown.value <= 0) {
            slowModeCooldown.value = 0;
            if (slowModeTimer) {
              clearInterval(slowModeTimer);
              slowModeTimer = null;
            }
          }
        }, 1000);
      });
    } else {
      // $xmppStatus is reset by XmppProvider on logout/unmount.
      clearTypingState();
    }
  }, { immediate: true });

  function addTypingUser(nick: string) {
    if (!typingUsers.value.includes(nick)) {
      typingUsers.value = [...typingUsers.value, nick];
    }
    // Auto-expire after 5 seconds (in case we miss a "paused" notification)
    const existing = typingTimers.get(nick);
    if (existing) clearTimeout(existing);
    typingTimers.set(
      nick,
      setTimeout(() => removeTypingUser(nick), 5000),
    );
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

  function notifyComposing() {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    if (!client || !channelId) return;

    if (lastChatState !== "composing") {
      lastChatState = "composing";
      void client.sendChatState(spaceId, channelId, "composing").catch(() => undefined);
    }

    // Reset the pause timer: if user stops typing for 3s, send "paused"
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (
        xmppClient.value !== client ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      )
        return;
      lastChatState = "paused";
      void client.sendChatState(spaceId, channelId, "paused").catch(() => undefined);
    }, 3000);
  }

  async function scrollToPinnedEdge() {
    await pinnedEdgeScroller.scrollToPinnedEdge();
  }

  async function scrollToPinnedEdgeAndPin() {
    return pinnedEdgeScroller.scrollToPinnedEdge({ settle: true });
  }

  function persistLastSeen(channelId: string, messageId: string) {
    setLastSeen(roomKey(channelId), messageId);
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

  function applyDisplayed(messageId: string, nick: string) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (!matchMessageId(m, messageId)) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) {
        existing.push(nick);
      }
      return { ...m, readBy: existing };
    });
  }

  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    void xmppClient.value.sendDisplayed(activeSpaceId.value ?? "", activeChannelId.value, targetId)
      .catch(() => undefined);
  }

  function applyReaction(messageId: string, nick: string, emojis: string[], senderId = nick) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.reactionTargetId !== messageId) return m;
      const reactionSenders = reactionSendersForUpdate(m, nick, senderId);
      removeSenderReactions(reactionSenders, nick, senderId);
      for (const emoji of emojis) {
        if (!reactionSenders[emoji]) reactionSenders[emoji] = {};
        reactionSenders[emoji][senderId] = nick;
      }
      const reactions = reactionsFromSenders(reactionSenders);
      const updated = { ...m };
      if (Object.keys(reactionSenders).length > 0) {
        updated.reactionSenders = reactionSenders;
        updated.reactions = reactions;
      } else {
        delete updated.reactionSenders;
        delete updated.reactions;
      }
      return updated;
    });
  }

  function reactionSendersForUpdate(
    message: TimelineMessage,
    nick?: string,
    senderId?: string,
  ): Record<string, Record<string, string>> {
    const reactionSenders: Record<string, Record<string, string>> = {};
    for (const [emoji, senders] of Object.entries(message.reactionSenders ?? {})) {
      reactionSenders[emoji] = { ...senders };
    }
    if (Object.keys(reactionSenders).length > 0) return reactionSenders;
    for (const [emoji, nicks] of Object.entries(message.reactions ?? {})) {
      reactionSenders[emoji] = {};
      for (const existingNick of nicks) {
        const legacySenderId = existingNick === nick && senderId ? senderId : existingNick;
        reactionSenders[emoji][legacySenderId] = existingNick;
      }
    }
    return reactionSenders;
  }

  function removeSenderReactions(
    reactionSenders: Record<string, Record<string, string>>,
    nick: string,
    senderId: string,
  ) {
    for (const key of Object.keys(reactionSenders)) {
      for (const existingSenderId of Object.keys(reactionSenders[key])) {
        if (
          existingSenderId === senderId ||
          existingSenderId === nick ||
          existingSenderId.endsWith(`/${nick}`)
        ) {
          delete reactionSenders[key][existingSenderId];
        }
      }
      if (Object.keys(reactionSenders[key]).length === 0) delete reactionSenders[key];
    }
  }

  function reactionsFromSenders(
    reactionSenders: Record<string, Record<string, string>>,
  ): Record<string, string[]> {
    return Object.fromEntries(
      Object.entries(reactionSenders).map(([emoji, senders]) => [emoji, Object.values(senders)]),
    );
  }

  function applyRetraction(retractsId: string) {
    messages.value = messages.value.map((m) =>
      matchMessageId(m, retractsId) ? { ...m, body: "", isRetracted: true } : m,
    );
  }

  function mucCorrectionSender(msg: Pick<LiveRoomMessage, "roomJid" | "nick" | "authorRealJid">): {
    authorJid: string;
    authorRealJid?: string;
  } {
    return {
      authorJid: `${msg.roomJid}/${msg.nick}`,
      ...(msg.authorRealJid ? { authorRealJid: msg.authorRealJid } : {}),
    };
  }

  function isSameMucCorrectionSender(
    target: TimelineMessage,
    correction: { authorJid: string; authorRealJid?: string },
  ): boolean {
    if ((target.authorOccupantJid ?? target.authorJid) !== correction.authorJid) return false;
    if (target.authorRealJid && correction.authorRealJid) {
      return barePeerJid(target.authorRealJid) === barePeerJid(correction.authorRealJid);
    }
    return true;
  }

  function applyCorrection(
    replacesId: string,
    newBody: string,
    correctionSender: { authorJid: string; authorRealJid?: string },
    markup?: MarkupSpan[],
    references?: MessageReference[],
    extensionAnnotations?: LiveRoomMessage["extensionAnnotations"],
  ) {
    const idx = messages.value.findIndex((m) =>
      matchMessageId(m, replacesId) && isSameMucCorrectionSender(m, correctionSender)
    );
    if (idx === -1) return;
    messages.value = messages.value.map((m) => {
      if (!matchMessageId(m, replacesId) || !isSameMucCorrectionSender(m, correctionSender)) return m;
      const updated: TimelineMessage = { ...m, body: newBody.trim(), isEdited: true };
      if (markup && markup.length > 0) {
        updated.markup = markup;
      } else {
        delete updated.markup;
      }
      if (references && references.length > 0) {
        updated.references = references;
      } else {
        delete updated.references;
      }
      if (extensionAnnotations && extensionAnnotations.length > 0) {
        updated.extensionAnnotations = extensionAnnotations;
      } else {
        delete updated.extensionAnnotations;
      }
      return updated;
    });
  }

  function mergeLiveMessage(msg: TimelineMessage) {
    // Check if this is a self-echo reconciling an optimistically-inserted
    // message. Match by ID first; otherwise, for our own sends, fall back
    // to body matching — but only against messages still awaiting echo
    // reconciliation (tracked via pendingEchoClientIds). Without that
    // constraint, sending the same text twice would let the second echo
    // re-target the already-reconciled first message.
    const existingById = [msg.id, ...(msg.wireIds ?? [])]
      .map((id) => findMessageById(messages.value, id))
      .find((message): message is TimelineMessage => !!message);
    const pendingSelfEcho = messages.value.find(
      (m) => pendingEchoClientIds.has(m.id) && m.isSelf && msg.isSelf && m.body === msg.body,
    );
    const preservedSelfEcho = [...messages.value].reverse().find(
      (m) =>
        m.isSelf
        && msg.isSelf
        && m.body === msg.body
        && !!m.deliveryStatus
        && m.deliveryStatus !== "delivered",
    );
    const existing = existingById ?? pendingSelfEcho ?? preservedSelfEcho;
    if (existing) {
      const wasPending = existing.id !== msg.id;
      // Reconcile the ID to the server-assigned one and merge authoritative
      // server-side enrichment while preserving local-only UI state.
      messages.value = messages.value.map((m) => {
        if (m.id !== existing.id) return m;
        const updated: TimelineMessage = {
          ...m,
          ...msg,
          ...mergeMessageIds(m, msg.id, msg.wireIds),
        };
        if (m.isSelf && msg.isSelf) {
          // A self-echo is authoritative evidence that the server accepted
          // the stanza, so it supersedes any prior "sending" or "failed"
          // optimistic state.
          updated.deliveryStatus = "delivered" as DeliveryStatus;
        }
        return updated;
      });
      messages.value = applyForumContext(messages.value);
      if (wasPending) pendingEchoClientIds.delete(existing.id);
      return;
    }
    const channelId = activeChannelId.value;
    messages.value = applyForumContext([...messages.value, msg]);
    // mergeLiveMessage always snaps to the active edge, so last-seen should advance
    // in lockstep regardless of the user's prior scroll position — if we're
    // scrolling them to the message, by definition they can see it.
    void scrollToPinnedEdgeAndPin();
    if (channelId && isFeedVisible(msg)) {
      persistLastSeen(channelId, msg.id);
    }
  }

  /**
   * XEP-0198: server acked our outbound stanza. The id here is the
   * client-assigned stanza ID. Promote the matching optimistic entry to
   * "delivered" even if the MUC self-echo hasn't arrived yet (the echo
   * will later reconcile the ID through mergeLiveMessage).
   */
  function onMessageAck(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: "delivered" as DeliveryStatus }
        : m,
    );
  }

  function onMessageQueueStatus(messageId: string, status: "queued" | "sending") {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: status as DeliveryStatus }
        : m,
    );
  }

  /**
   * XEP-0198: the XMPP client gave up on the stanza (resume failed or no
   * resumable transport). Mark the message as failed so the UI can
   * surface a retry affordance. Kept in place so the user can see what
   * did not go through.
   */
  function onMessageDeliveryFailure(messageId: string) {
    messages.value = messages.value.map((m) =>
      m.id === messageId && m.isSelf && m.deliveryStatus !== "delivered"
        ? { ...m, deliveryStatus: "failed" as DeliveryStatus }
        : m,
    );
  }

  /**
   * On a fresh XMPP session (resume failed or first connect after a drop),
   * refetch MAM to close any message gap for the current channel. Local
   * optimistic sends (sending/failed) are preserved across the reload so
   * the UI doesn't drop unsent entries the user can still retry.
   * Resumed sessions never call this — the server replays everything.
   */
  function onSessionLifecycle(event: SessionLifecycleEvent) {
    if (event.type !== "fresh") return;
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
      await loadMessages(spaceId ?? "", channelId, 0, metadataSeed);
      if (
        preserved.length === 0 ||
        (activeSpaceId.value ?? "") !== (spaceId ?? "") ||
        activeChannelId.value !== channelId
      )
        return;
      const toAppend = preserved.filter((m) => !findMessageById(messages.value, m.id));
      if (toAppend.length > 0) messages.value = [...messages.value, ...toAppend];
    })();
  }

  // Matches ContentArea.feedMessages: thread replies (messages with a
  // threadId that isn't their own id) are hidden, so the last-seen anchor
  // and the "New messages" divider must be computed against this predicate.
  const isFeedVisible = isFeedTimelineMessage;

  function buildTimelineFromMamResults(
    mamResults: LiveRoomMessage[],
    existing: TimelineMessage[] = [],
    options: TimelineBuildOptions = {},
  ): TimelineMessage[] {
    const regularMessages: LiveRoomMessage[] = [];
    const reactionUpdates: { targetId: string; nick: string; senderId: string; emojis: string[] }[] = [];
    const retractionUpdates: string[] = [];
    const correctionUpdates: {
      targetId: string;
      correctionSender: { authorJid: string; authorRealJid?: string };
      body: string;
      markup?: MarkupSpan[];
      references?: MessageReference[];
      extensionAnnotations?: LiveRoomMessage["extensionAnnotations"];
    }[] = [];

    for (const msg of mamResults) {
      if (msg._reactionTarget && msg._reactionEmojis) {
        reactionUpdates.push({
          targetId: msg._reactionTarget,
          nick: msg.nick,
          senderId: msg._reactionSenderId ?? `${msg.roomJid}/${msg.nick}`,
          emojis: msg._reactionEmojis,
        });
      } else if (msg.retractsId) {
        retractionUpdates.push(msg.retractsId);
      } else if (msg.replacesId) {
        correctionUpdates.push({
          targetId: msg.replacesId,
          correctionSender: mucCorrectionSender(msg),
          body: msg.body,
          markup: msg.markup,
          references: msg.references,
          extensionAnnotations: msg.extensionAnnotations,
        });
      } else if (
        msg.body
        || (msg.sharedFiles && msg.sharedFiles.length > 0)
        || msg.isSticker
        || msg.threadId
        || msg.replyTo
        || msg.forumPostKind
        || (msg.extensionAnnotations && msg.extensionAnnotations.length > 0)
      ) {
        regularMessages.push(msg);
      }
    }

    const byId = new Map<string, TimelineMessage>();
    for (const message of existing) {
      indexMessageByIds(byId, message);
    }
    const timeline = options.seedExistingOnly ? [] : [...existing];
    for (const raw of regularMessages) {
      const tm = mapLiveRoomMessageToTimeline(session.value!, raw, (id) => byId.get(id));
      const existingMessage = [tm.id, ...(tm.wireIds ?? [])]
        .map((id) => byId.get(id))
        .find((message): message is TimelineMessage => !!message);
      if (existingMessage) {
        const merged = options.seedExistingOnly
          ? mergeMissingThreadMetadata(tm, existingMessage)
          : mergeMissingThreadMetadata(existingMessage, tm);
        if (options.seedExistingOnly) {
          indexMessageByIds(byId, merged);
          timeline.push(merged);
        } else if (merged !== existingMessage) {
          const index = timeline.indexOf(existingMessage);
          if (index !== -1) timeline[index] = merged;
          indexMessageByIds(byId, merged);
        }
        continue;
      }
      indexMessageByIds(byId, tm);
      timeline.push(tm);
    }

    for (const update of correctionUpdates) {
      const target = findMessageById(timeline, update.targetId);
      if (!target || !isSameMucCorrectionSender(target, update.correctionSender)) continue;
      target.body = update.body;
      target.isEdited = true;
      if (update.markup && update.markup.length > 0) target.markup = update.markup;
      else delete target.markup;
      if (update.references && update.references.length > 0) target.references = update.references;
      else delete target.references;
      if (update.extensionAnnotations && update.extensionAnnotations.length > 0) {
        target.extensionAnnotations = update.extensionAnnotations;
      } else {
        delete target.extensionAnnotations;
      }
    }

    for (const retractsId of retractionUpdates) {
      const target = findMessageById(timeline, retractsId);
      if (!target) continue;
      target.body = "";
      target.isRetracted = true;
    }

    for (const update of reactionUpdates) {
      const target = timeline.find((message) => message.reactionTargetId === update.targetId);
      if (!target) continue;
      const reactionSenders = reactionSendersForUpdate(target, update.nick, update.senderId);
      removeSenderReactions(reactionSenders, update.nick, update.senderId);
      for (const emoji of update.emojis) {
        if (!reactionSenders[emoji]) reactionSenders[emoji] = {};
        reactionSenders[emoji][update.senderId] = update.nick;
      }
      const reactions = reactionsFromSenders(reactionSenders);
      if (Object.keys(reactionSenders).length > 0) {
        target.reactionSenders = reactionSenders;
        target.reactions = reactions;
      } else {
        delete target.reactionSenders;
        delete target.reactions;
      }
    }

    return applyForumContext(timeline.sort((a, b) => a.createdAt.localeCompare(b.createdAt)), channelIsForum.value);
  }

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
    searchRequestId++;
    searchQuery.value = "";
    searchResults.value = [];
    isSearching.value = false;
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
      // XEP-0313: Load message history via MAM (XMPP-native)
      const page = xmppClient.value && "queryMamPage" in xmppClient.value
        ? await xmppClient.value.queryMamPage(spaceId, channelId, 100, { type: "latest" })
        : null;
      const mamResults = page
        ? page.messages
        : xmppClient.value
          ? await xmppClient.value.queryMam(spaceId, channelId, 100)
          : [];

      if (
        requestId !== messageRequestId ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }

      oldestArchiveId = page?.firstArchiveId ?? mamResults[0]?.id ?? null;
      hasOlderMessages.value = page ? !page.complete && !!page.firstArchiveId : mamResults.length >= 100;
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
      const page = await client.queryMamPage(spaceId, channelId, 100, { type: "before", before });
      if (!isCurrentRequest()) return;
      oldestArchiveId = page.firstArchiveId ?? oldestArchiveId;
      hasOlderMessages.value = !page.complete && !!page.firstArchiveId && page.firstArchiveId !== before;
      const withoutQueued = messages.value.filter((m) => !(m.isSelf && m.deliveryStatus === "queued"));
      const roomJid = roomJidForChannel(channelId);
      if (!roomJid) return;
      messages.value = appendQueuedMessages(buildTimelineFromMamResults(page.messages, withoutQueued), roomJid);
      await nextTick();
      if (el && !isTopPinnedScrollDirection(scrollDirection.value)) {
        el.scrollTop = previousTop + (el.scrollHeight - previousHeight);
      }
    } catch (e) {
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
      const page = await client.queryMamPage(spaceId, channelId, 100, { type: "before", before });
      if (
        requestId !== messageRequestId ||
        xmppClient.value !== client ||
        (activeSpaceId.value ?? "") !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        return false;
      }
      const nextBefore = page.firstArchiveId ?? previousBefore;
      oldestArchiveId = nextBefore;
      hasOlderMessages.value = !page.complete && !!page.firstArchiveId && page.firstArchiveId !== previousBefore;
      const withoutQueued = messages.value.filter((m) => !(m.isSelf && m.deliveryStatus === "queued"));
      messages.value = appendQueuedMessages(
        buildTimelineFromMamResults(page.messages, withoutQueued),
        roomJid,
      );
      if (findMessageById(messages.value, messageId)) return true;
      if (!page.firstArchiveId || page.firstArchiveId === previousBefore || page.complete) break;
      before = nextBefore;
    }
    return !!findMessageById(messages.value, messageId);
  }

  /**
   * Backfill a thread via XEP-0313 MAM filtered by thread id. Returns every
   * archived reply whose `<thread>` element matches `threadId`. The thread
   * root does not carry `<thread>` (threads start when someone replies into
   * one), so MAM-by-thread never includes it — the panel resolves the root
   * separately from the loaded channel window.
   */
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
        ? await client.queryMamThreadPage(spaceId ?? "", channelId, threadId, 100, { type: "latest" })
        : null;
      results = page ? page.messages : await client.queryMamByThread(spaceId ?? "", channelId, threadId, 100);
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
    if (page?.firstArchiveId) oldestThreadArchiveIds.set(threadId, page.firstArchiveId);
    threadHasOlder.value = {
      ...threadHasOlder.value,
      [threadId]: page ? !page.complete && !!page.firstArchiveId : results.length >= 100,
    };
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
      const page = await client.queryMamThreadPage(spaceId ?? "", channelId, threadId, 100, { type: "before", before });
      if (!isCurrentRequest()) return;
      if (page.firstArchiveId) oldestThreadArchiveIds.set(threadId, page.firstArchiveId);
      threadHasOlder.value = {
        ...threadHasOlder.value,
        [threadId]: !page.complete && !!page.firstArchiveId && page.firstArchiveId !== before,
      };
      messages.value = buildTimelineFromMamResults(page.messages, messages.value);
    } catch (e) {
      if (isCurrentRequest()) actionError.value = normalizeError(e);
    } finally {
      const next = new Set(loadingOlderThreadIds.value);
      next.delete(threadId);
      loadingOlderThreadIds.value = next;
    }
  }

  async function selectChannel(channelId: string) {
    messages.value = [];
    clearTypingState();
    await loadMessages(activeSpaceId.value ?? "", channelId);
  }

  async function sendMessage(
    body?: string,
    markup?: MarkupSpan[],
    references?: MessageReference[],
    files?: Array<File | Blob>,
    replyTo?: { id: string; author: string; body?: string },
    forumTitleOrThreadOverride?: string | { threadId: string; parentThreadId?: string },
  ) {
    const bodyText = body ?? draft.value;
    // markup !== undefined means this came from the rich editor (composer send)
    // undefined means it came from a programmatic send (GIF, etc.)
    const fromComposer = markup !== undefined;
    const threadOverride = typeof forumTitleOrThreadOverride === "string"
      ? undefined
      : forumTitleOrThreadOverride;
    const forumTitle = typeof forumTitleOrThreadOverride === "string"
      ? forumTitleOrThreadOverride
      : undefined;
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    const hasFiles = !!files && files.length > 0;
    const isForumPost = channelIsForum.value && !replyTo;
    const resolvedForumTitle = (forumTitle ?? forumPostTitle.value).trim();
    const hasThreadMetadataIntent = !!threadOverride?.threadId.trim();
    const hasForumMetadataIntent = (isForumPost && !!resolvedForumTitle) || (channelIsForum.value && !!replyTo);
    if (!client || !channelId) return;
    if (!bodyText.trim() && !hasFiles && !hasThreadMetadataIntent && !hasForumMetadataIntent) return;
    if (isForumPost && !resolvedForumTitle) {
      actionError.value = "Add a title before posting to this forum.";
      return;
    }

    if (hasFiles) {
      for (const f of files!) {
        if (f.size > MAX_FILE_UPLOAD_BYTES) {
          actionError.value = `File too large (${(f.size / 1024 / 1024).toFixed(1)} MB). Maximum upload size is 10 MB.`;
          return;
        }
      }
    }

    isSending.value = true;
    clearActionError();

    try {
      let attachments: OutboundFileAttachment[] | undefined;
      if (hasFiles) {
        const filenames = files!.map((f) => (f instanceof File ? f.name : `attachment-${Date.now()}.bin`));
        uploadProgress.value = { uploading: true, progress: 0, filename: filenames[0] };
        attachments = await client.uploadAttachments(files!, (overall, idx) => {
          uploadProgress.value = {
            uploading: true,
            progress: overall.total > 0 ? overall.loaded / overall.total : 0,
            filename: filenames[idx] ?? "",
          };
        });
      }

      const parent = replyTo ? findMessageById(messages.value, replyTo.id) : undefined;
      // XEP-0461 §3.2: groupchat replies MUST quote the room-assigned
      // XEP-0359 stanza-id. Without one, "messages without one cannot be
      // replied to"; refuse the send rather than leak a non-conformant id.
      if (replyTo && parent && !parent.replyableId) {
        actionError.value =
          "This message can't be replied to (no room stanza-id). Try reloading the channel.";
        isSending.value = false;
        return;
      }
      const wireReplyTo = replyTo && parent && parent.replyableId
        ? {
            id: parent.replyableId,
            author: parent.authorOccupantJid ?? parent.authorJid ?? replyTo.author,
            ...(replyTo.body ? { body: replyTo.body } : {}),
          }
        : undefined;
      // Thread membership is explicit via threadOverride, except forum replies,
      // which derive their thread from the replied-to topic/message.
      const threadId = threadOverride?.threadId
        ?? (channelIsForum.value && parent ? (parent.threadId ?? parent.id) : undefined);
      const parentThreadId = threadOverride?.parentThreadId;
      const threadCreate = isForumPost ? { title: resolvedForumTitle } : undefined;
      const threadReply = channelIsForum.value && replyTo && threadId
        ? { threadId }
        : undefined;
      const result = await client.sendGroupMessage(spaceId, channelId, bodyText, {
        markup,
        references,
        files: attachments,
        ...(wireReplyTo ? { replyTo: wireReplyTo } : {}),
        ...(threadId ? { threadId } : {}),
        ...(parentThreadId ? { parentThreadId } : {}),
        ...(threadCreate ? { threadCreate } : {}),
        ...(threadReply ? { threadReply } : {}),
        ...(mentionJidsByNick ? { mentionJidsByNick: mentionJidsByNick.value } : {}),
      });
      const msgId = result?.id ?? null;
      const isStillCurrentChannel =
        xmppClient.value === client &&
        (activeSpaceId.value ?? "") === spaceId &&
        activeChannelId.value === channelId;

      if (msgId && session.value && isStillCurrentChannel) {
        // Optimistic insert: show message immediately with "sending" status
        const optimistic: TimelineMessage = {
          id: msgId,
          correctionTargetId: msgId,
          author: session.value.username,
          authorJid: `${currentRoomJid.value}/${session.value.username}`,
          body: bodyText || (attachments?.[0]?.url ?? ""),
          createdAt: new Date().toISOString(),
          isSelf: true,
          deliveryStatus: (result?.state ?? "sending") as DeliveryStatus,
          ...(markup && markup.length > 0 ? { markup } : {}),
          ...(references && references.length > 0 ? { references } : {}),
        };
        if (replyTo && parent && parent.replyableId) {
          // Mirror the wire reply id on the optimistic insert so the local
          // chip and the eventual MAM round-trip resolve to the same id.
          optimistic.replyTo = {
            id: parent.replyableId,
            author: replyTo.author,
            ...(replyTo.body ? { preview: replyTo.body } : {}),
          };
        }
        if (threadId) optimistic.threadId = threadId;
        if (parentThreadId) optimistic.parentThreadId = parentThreadId;
        if (threadCreate) {
          optimistic.threadId = msgId;
          optimistic.forumPostKind = "topic";
          optimistic.forumTitle = threadCreate.title;
          optimistic.forumThreadTitle = threadCreate.title;
        } else if (threadReply) {
          optimistic.forumPostKind = "reply";
          const threadTitle = parent?.forumTitle ?? parent?.forumThreadTitle;
          if (threadTitle) optimistic.forumThreadTitle = threadTitle;
        }
        if (attachments && attachments.length > 0) {
          optimistic.sharedFiles = attachments.map((a) => ({
            url: a.url,
            name: a.name,
            mediaType: a.mediaType,
            size: a.size,
            ...(a.width ? { width: a.width } : {}),
            ...(a.height ? { height: a.height } : {}),
            disposition: inferredFileDisposition(a.mediaType, a.name ?? a.url),
            ...(a.encrypted ? { encrypted: a.encrypted } : {}),
          }));
        }
        pendingEchoClientIds.add(msgId);
        messages.value = applyForumContext([...messages.value, optimistic]);
        void scrollToPinnedEdgeAndPin();
      }

      // Clear draft on successful send (triggers ChatEditor clear via watcher)
      if (fromComposer && isStillCurrentChannel) {
        draft.value = "";
        if (threadCreate) forumPostTitle.value = "";
      }
      // Send "active" state after sending a message (stops composing indicator)
      if (isStillCurrentChannel) {
        if (composingTimeout) {
          clearTimeout(composingTimeout);
          composingTimeout = null;
        }
        lastChatState = "active";
      }
      if (result?.state === "sending") {
        void client.sendChatState(spaceId, channelId, "active").catch(() => undefined);
      }
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
      uploadProgress.value = { uploading: false, progress: 0, filename: "" };
    }
  }

  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activeChannelId.value || !session.value)
      return;

    // Compute the new reaction set for this user
    const msg = findMessageById(messages.value, messageId);
    const targetId = msg?.reactionTargetId;
    if (!targetId) return;
    const myNick = session.value.username;
    const currentReactions = msg?.reactions ?? {};

    // Gather all emojis this user currently has on this message
    const myEmojis = new Set<string>();
    for (const [e, nicks] of Object.entries(currentReactions)) {
      if (nicks.includes(myNick)) myEmojis.add(e);
    }

    // Toggle the emoji
    if (myEmojis.has(emoji)) {
      myEmojis.delete(emoji);
    } else {
      myEmojis.add(emoji);
    }

    try {
      await xmppClient.value.sendReaction(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        targetId,
        [...myEmojis],
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.replyableId;

    clearActionError();
    if (!targetId) {
      actionError.value =
        "This message can't be retracted (no room stanza-id). Try reloading the channel.";
      return;
    }

    try {
      await xmppClient.value.sendRetraction(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        targetId,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function moderateMessage(messageId: string, reason?: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.replyableId;

    clearActionError();
    if (!targetId) {
      actionError.value =
        "This message can't be moderated (no room stanza-id). Try reloading the channel.";
      return;
    }

    try {
      await xmppClient.value.sendModeration(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        targetId,
        reason,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function invokeExtensionAction(action: ExtensionAnnotationAction) {
    const client = xmppClient.value;
    if (!client) {
      const error = new Error("XMPP session is not ready.");
      actionError.value = normalizeError(error);
      throw error;
    }
    if (!action.launch) {
      const error = new Error("This extension action is missing launch metadata.");
      actionError.value = normalizeError(error);
      throw error;
    }
    clearActionError();
    try {
      return await client.invokeExtensionLaunch(action.launch);
    } catch (e) {
      actionError.value = normalizeError(e);
      throw e;
    }
  }

  async function editMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]) {
    if (!xmppClient.value || !activeChannelId.value || !newBody.trim())
      return;
    const message = findMessageById(messages.value, messageId);
    const targetId = message?.correctionTargetId ?? messageId;

    clearActionError();

    try {
      await xmppClient.value.sendCorrection(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        newBody,
        targetId,
        markup,
        references,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function disconnect() {
    messageRequestId++;
    searchRequestId++;
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    initialLatestPagePinned = false;
    oldestArchiveId = null;
    hasOlderMessages.value = true;
    isLoadingOlderMessages.value = false;
    // $xmppStatus is authoritative and owned by XmppProvider; do not write it here.
    clearTypingState();
    isLoadingMessages.value = false;
    isSearching.value = false;
    searchQuery.value = "";
    searchResults.value = [];
    firstUnseenId.value = null;
  }

  function clearMessages() {
    messageRequestId++;
    searchRequestId++;
    pinnedEdgeScroller.disconnect();
    pendingEchoClientIds.clear();
    initialLatestPagePinned = false;
    oldestArchiveId = null;
    hasOlderMessages.value = true;
    isLoadingOlderMessages.value = false;
    messages.value = [];
    isLoadingMessages.value = false;
    searchQuery.value = "";
    searchResults.value = [];
    isSearching.value = false;
    clearTypingState();
    firstUnseenId.value = null;
  }

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    if (!client || !channelId) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    searchQuery.value = trimmed;
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    clearActionError();
    try {
      const results = await client.searchMessages(spaceId, channelId, trimmed);
      if (
        requestId === searchRequestId &&
        xmppClient.value === client &&
        (activeSpaceId.value ?? "") === spaceId &&
        activeChannelId.value === channelId
      ) {
        searchResults.value = results;
      }
    } catch (e) {
      if (requestId === searchRequestId) {
        searchResults.value = [];
        actionError.value = normalizeError(e);
      }
    } finally {
      if (requestId === searchRequestId) {
        isSearching.value = false;
      }
    }
  }

  function clearSearch() {
    searchRequestId++;
    searchQuery.value = "";
    searchResults.value = [];
    isSearching.value = false;
  }

  function clearChannelActivity(roomJid: string) {
    const next = new Set(activeChannels.value);
    next.delete(roomJid);
    activeChannels.value = next;
  }

  watch(scrollDirection, () => {
    void alignTimelineToPreference();
  });

  watch(
    [timelineEl, timelineEdgeScroller],
    async ([el, edgeScroller]) => {
      if (!el || !edgeScroller || isLoadingMessages.value) return;
      if (!messages.value.some(isFeedVisible)) return;
      const requestId = messageRequestId;
      const spaceId = activeSpaceId.value ?? "";
      const channelId = activeChannelId.value;
      const pinned = await scrollToPinnedEdgeAndPin();
      if (
        pinned &&
        requestId === messageRequestId &&
        (activeSpaceId.value ?? "") === spaceId &&
        activeChannelId.value === channelId &&
        messages.value.some(isFeedVisible)
      ) {
        initialLatestPagePinned = true;
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
    typingUsers,
    roomHats,
    roomPresence,
    roomLastSeen,
    slowModeCooldown,
    loadMessages,
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
    disconnect,
    clearMessages,
    activeChannels,
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
    onMessageQueueStatus,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
  };
}
