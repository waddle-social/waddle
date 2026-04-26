import { ref, computed, nextTick, watch, type Ref } from "vue";
import { useStore } from "@nanostores/vue";
import type { ChannelSummary } from "@/lib/chat-types";
import { isForumChannel } from "@/lib/channel-types";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  barePeerJid,
  roomBareJidFor,
  type LiveRoomMessage,
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
import { getPinnedScrollTop, isTopPinnedScrollDirection } from "@/lib/scroll-direction";
import { getLastSeen, roomKey, setLastSeen } from "@/lib/last-seen-store";
import {
  listQueuedRoomMessages,
  type PersistedQueuedRoomMessage,
} from "@/lib/outbound-queue-store";
import { mentionMatchesUsername } from "@/lib/mentions";
import { useScrollDirection } from "@/composables/useScrollDirection";

export function fromLiveMessage(
  session: WaddleSession,
  msg: LiveRoomMessage,
  parentLookup?: (id: string) => { body?: string } | undefined,
): TimelineMessage {
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    authorJid: msg.authorRealJid ?? `${msg.roomJid}/${msg.nick}`,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: msg.nick === session.username,
  };
  if (msg.authorRealJid) tm.authorRealJid = msg.authorRealJid;
  if (msg.wireIds && msg.wireIds.length > 0) {
    tm.wireIds = msg.wireIds;
  }
  if (msg.mentions && msg.mentions.length > 0) {
    tm.mentions = msg.mentions;
  }
  if (msg.sharedFiles && msg.sharedFiles.length > 0) {
    tm.sharedFiles = msg.sharedFiles;
  }
  if (msg.githubEmbeds && msg.githubEmbeds.length > 0) {
    tm.githubEmbeds = msg.githubEmbeds;
  }
  if (msg.isSticker) {
    tm.isSticker = true;
  }
  if (msg.broadcastMention) {
    tm.broadcastMention = msg.broadcastMention;
  }
  if (msg.markup && msg.markup.length > 0) {
    tm.markup = msg.markup;
  }
  if (msg.references && msg.references.length > 0) {
    tm.references = msg.references;
  }
  if (msg.replyTo) {
    const parent = parentLookup?.(msg.replyTo.id);
    tm.replyTo = {
      id: msg.replyTo.id,
      ...(msg.replyTo.author ? { author: msg.replyTo.author } : {}),
      ...(parent?.body ? { preview: parent.body } : {}),
    };
  }
  if (msg.threadId) tm.threadId = msg.threadId;
  if (msg.parentThreadId) tm.parentThreadId = msg.parentThreadId;
  if (msg.forumPostKind) tm.forumPostKind = msg.forumPostKind;
  if (msg.forumTitle) tm.forumTitle = msg.forumTitle;
  if (msg.forumThreadTitle) tm.forumThreadTitle = msg.forumThreadTitle;
  return tm;
}

function applyForumContext(list: TimelineMessage[]): TimelineMessage[] {
  const threadTitles = new Map<string, string>();

  for (const message of list) {
    if (message.forumPostKind === "topic" && message.forumTitle) {
      threadTitles.set(message.threadId ?? message.id, message.forumTitle);
    }
  }

  return list.map((message) => {
    const threadId = message.threadId ?? (message.forumPostKind === "topic" ? message.id : undefined);
    const threadTitle = threadId ? threadTitles.get(threadId) : undefined;
    const next: TimelineMessage = { ...message };
    let changed = false;

    if (message.forumPostKind === "topic") {
      if (threadId && message.threadId !== threadId) {
        next.threadId = threadId;
        changed = true;
      }
      if (message.forumTitle && message.forumThreadTitle !== message.forumTitle) {
        next.forumThreadTitle = message.forumTitle;
        changed = true;
      }
    } else if (threadId && threadTitle) {
      if (!message.forumPostKind && message.id !== threadId) {
        next.forumPostKind = "reply";
        changed = true;
      }
      if (message.forumThreadTitle !== threadTitle) {
        next.forumThreadTitle = threadTitle;
        changed = true;
      }
    }

    return changed ? next : message;
  });
}

function queuedRoomMessageToTimeline(
  session: WaddleSession,
  roomJid: string,
  queued: PersistedQueuedRoomMessage,
): TimelineMessage {
  const message: TimelineMessage = {
    id: queued.id,
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

export function formatStamp(value: string) {
  return new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
    month: "short",
    day: "numeric",
  }).format(new Date(value));
}

export function formatTimeOfDay(value: string) {
  return new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

const DAY_DIVIDER_FORMATTER = new Intl.DateTimeFormat("en", {
  weekday: "short",
  month: "short",
  day: "numeric",
});

function startOfDay(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

export function formatDayDivider(value: string) {
  const date = new Date(value);
  const today = startOfDay(new Date());
  const target = startOfDay(date);
  const oneDay = 24 * 60 * 60 * 1000;
  if (target === today) return "Today";
  if (target === today - oneDay) return "Yesterday";
  // Older messages get a full weekday + month + day banner.
  return DAY_DIVIDER_FORMATTER.format(date);
}

export function isSameDay(a: string, b: string): boolean {
  return startOfDay(new Date(a)) === startOfDay(new Date(b));
}

export function useMessaging(
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
  const { mode: scrollDirection } = useScrollDirection();
  // xmppStatus is owned by $xmppStatus (written from XmppProvider, which is
  // persisted across route changes). Reading via useStore keeps the composable
  // in sync with the authoritative snapshot on every mount.
  const xmppStatus = useStore($xmppStatus);

  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const forumPostTitle = ref("");
  const isLoadingMessages = ref(false);
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const typingUsers = ref<string[]>([]);
  const roomHats = ref<RoomHats>({});
  const roomPresence = ref<RoomPresence>({});
  const roomLastSeen = ref<Record<string, number>>({});
  const slowModeCooldown = ref(0);
  const uploadProgress = ref({ uploading: false, progress: 0, filename: "" });
  const firstUnseenId = ref<string | null>(null);
  const activeChannels = ref<Set<string>>(new Set());
  const lastMentionActivity = ref<RoomActivityEvent | null>(null);
  const roomAvatarHashes = ref<Record<string, string>>({});
  const searchQuery = ref("");
  const searchResults = ref<{ id: string; nick: string; body: string; createdAt: string }[]>([]);
  const isSearching = ref(false);
  let slowModeTimer: ReturnType<typeof setInterval> | null = null;

  let messageRequestId = 0;
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
    if (!session.value || !activeSpaceId.value || !activeChannelId.value) return null;
    return roomBareJidFor(session.value, activeChannelId.value);
  });
  const channelIsForum = computed(() => isForumChannel(currentChannel.value));

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
          applyCorrection(msg.replacesId, msg.body, msg.markup, msg.references, msg.githubEmbeds);
          return;
        }

        mergeLiveMessage(
          fromLiveMessage(session.value!, msg, (id) => findMessageById(messages.value, id)),
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
        applyReaction(event.messageId, event.nick, event.emojis);
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
        activeSpaceId.value !== spaceId ||
        activeChannelId.value !== channelId
      )
        return;
      lastChatState = "paused";
      void client.sendChatState(spaceId, channelId, "paused").catch(() => undefined);
    }, 3000);
  }

  async function scrollToPinnedEdge() {
    await nextTick();
    await nextTick();
    const el = timelineEl.value;
    if (!el) return;
    el.scrollTop = getPinnedScrollTop(el, scrollDirection.value);
  }

  // Initial-load variant: after scrolling, re-pin for ~500ms via a
  // ResizeObserver so late layout (images, avatars, markup reflow) doesn't
  // strand the user above the newest message. Not used per-message —
  // ResizeObserver allocation per live message would be O(n) overhead.
  async function scrollToPinnedEdgeAndPin() {
    await scrollToPinnedEdge();
    const el = timelineEl.value;
    if (!el || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      el.scrollTop = getPinnedScrollTop(el, scrollDirection.value);
    });
    observer.observe(el);
    for (const child of Array.from(el.children)) {
      observer.observe(child);
    }
    setTimeout(() => observer.disconnect(), 500);
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
    if (!xmppClient.value || !activeSpaceId.value || !activeChannelId.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;
    void xmppClient.value.sendDisplayed(activeSpaceId.value, activeChannelId.value, targetId)
      .catch(() => undefined);
  }

  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (!matchMessageId(m, messageId)) return m;
      const existing: Record<string, string[]> = m.reactions ? { ...m.reactions } : {};
      // Remove this nick from all existing emoji lists
      for (const key of Object.keys(existing)) {
        existing[key] = (existing[key] ?? []).filter((n) => n !== nick);
        if (existing[key].length === 0) delete existing[key];
      }
      // Add the nick to each new emoji
      for (const emoji of emojis) {
        if (!existing[emoji]) existing[emoji] = [];
        existing[emoji].push(nick);
      }
      const updated = { ...m };
      if (Object.keys(existing).length > 0) {
        updated.reactions = existing;
      } else {
        delete updated.reactions;
      }
      return updated;
    });
  }

  function applyRetraction(retractsId: string) {
    messages.value = messages.value.map((m) =>
      matchMessageId(m, retractsId) ? { ...m, body: "", isRetracted: true } : m,
    );
  }

  function applyCorrection(
    replacesId: string,
    newBody: string,
    markup?: MarkupSpan[],
    references?: MessageReference[],
    githubEmbeds?: LiveRoomMessage["githubEmbeds"],
  ) {
    const idx = messages.value.findIndex((m) => matchMessageId(m, replacesId));
    if (idx === -1) return;
    messages.value = messages.value.map((m) => {
      if (!matchMessageId(m, replacesId)) return m;
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
      if (githubEmbeds && githubEmbeds.length > 0) {
        updated.githubEmbeds = githubEmbeds;
      } else if (m.githubEmbeds?.length) {
        const newBodyText = newBody.trim();
        const surviving = m.githubEmbeds.filter((e) => newBodyText.includes(e.url));
        if (surviving.length > 0) {
          updated.githubEmbeds = surviving;
        } else {
          delete updated.githubEmbeds;
        }
      } else {
        delete updated.githubEmbeds;
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
    const spaceId = activeSpaceId.value;
    const channelId = activeChannelId.value;
    messages.value = applyForumContext([...messages.value, msg]);
    // mergeLiveMessage always snaps to the active edge, so last-seen should advance
    // in lockstep regardless of the user's prior scroll position — if we're
    // scrolling them to the message, by definition they can see it.
    void scrollToPinnedEdge();
    if (spaceId && channelId && isFeedVisible(msg)) {
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
   * XEP-0198: stanza.js gave up on the stanza (resume failed or no
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
    if (!spaceId || !channelId) return;
    // Only catch up if we had already loaded this channel; otherwise the
    // standard loadMessages call on channel-select handles it.
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
      await loadMessages(spaceId, channelId);
      if (
        preserved.length === 0 ||
        activeSpaceId.value !== spaceId ||
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
  function isFeedVisible(m: TimelineMessage): boolean {
    return !m.threadId || m.id === m.threadId;
  }

  async function loadMessages(spaceId: string, channelId: string) {
    if (!session.value) return;

    const requestId = ++messageRequestId;
    const roomJid = roomBareJidFor(session.value, channelId);
    isLoadingMessages.value = true;
    // Reset the divider anchor up-front: a previous conversation's id could
    // coincidentally match a message in the new timeline, and an aborted
    // request (requestId mismatch) would otherwise leave stale state.
    firstUnseenId.value = null;
    clearActionError();
    pendingEchoClientIds.clear();
    messages.value = appendQueuedMessages([], roomJid);

    try {
      // XEP-0313: Load message history via MAM (XMPP-native)
      const mamResults = xmppClient.value
        ? await xmppClient.value.queryMam(spaceId, channelId, 100)
        : [];

      if (
        requestId !== messageRequestId ||
        activeSpaceId.value !== spaceId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }

      // Separate regular messages from timeline updates
      const regularMessages: LiveRoomMessage[] = [];
      const reactionUpdates: { targetId: string; nick: string; emojis: string[] }[] = [];
      const retractionUpdates: string[] = [];
      const correctionUpdates: {
        targetId: string;
        body: string;
        markup?: MarkupSpan[];
        references?: MessageReference[];
        githubEmbeds?: LiveRoomMessage["githubEmbeds"];
      }[] = [];

      for (const msg of mamResults) {
        if (msg._reactionTarget && msg._reactionEmojis) {
          reactionUpdates.push({
            targetId: msg._reactionTarget,
            nick: msg.nick,
            emojis: msg._reactionEmojis,
          });
        } else if (msg.retractsId) {
          retractionUpdates.push(msg.retractsId);
        } else if (msg.replacesId) {
          correctionUpdates.push({
            targetId: msg.replacesId,
            body: msg.body,
            markup: msg.markup,
            references: msg.references,
            githubEmbeds: msg.githubEmbeds,
          });
        } else if (msg.body || (msg.sharedFiles && msg.sharedFiles.length > 0) || msg.isSticker || (msg.githubEmbeds && msg.githubEmbeds.length > 0)) {
          regularMessages.push(msg);
        }
      }

      // Convert to timeline messages; accumulate in a map so replies that land
      // after their parent in the MAM batch can resolve a preview.
      const byId = new Map<string, TimelineMessage>();
      const timeline = regularMessages.map((m) => {
        const tm = fromLiveMessage(session.value!, m, (id) => byId.get(id));
        indexMessageByIds(byId, tm);
        return tm;
      });

      for (const update of correctionUpdates) {
        const target = findMessageById(timeline, update.targetId);
        if (!target) continue;
        target.body = update.body;
        target.isEdited = true;
        if (update.markup && update.markup.length > 0) {
          target.markup = update.markup;
        } else {
          delete target.markup;
        }
        if (update.references && update.references.length > 0) {
          target.references = update.references;
        } else {
          delete target.references;
        }
        if (update.githubEmbeds && update.githubEmbeds.length > 0) {
          target.githubEmbeds = update.githubEmbeds;
        } else if (target.githubEmbeds?.length) {
          const surviving = target.githubEmbeds.filter((e) => update.body.includes(e.url));
          if (surviving.length > 0) {
            target.githubEmbeds = surviving;
          } else {
            delete target.githubEmbeds;
          }
        } else {
          delete target.githubEmbeds;
        }
      }

      for (const retractsId of retractionUpdates) {
        const target = findMessageById(timeline, retractsId);
        if (!target) continue;
        target.body = "";
        target.isRetracted = true;
      }

      // Apply reactions from MAM history
      for (const update of reactionUpdates) {
        const target = findMessageById(timeline, update.targetId);
        if (target) {
          const reactions: Record<string, string[]> = target.reactions ? { ...target.reactions } : {};
          for (const emoji of update.emojis) {
            if (!reactions[emoji]) reactions[emoji] = [];
            if (!reactions[emoji].includes(update.nick)) {
              reactions[emoji].push(update.nick);
            }
          }
          target.reactions = reactions;
        }
      }

      const timelineWithQueue = appendQueuedMessages(applyForumContext(timeline), roomJid);
      messages.value = timelineWithQueue;
      if (requestId === messageRequestId) {
        isLoadingMessages.value = false;
      }

      // Restore scroll position: if we have a last-seen anchor that's still
      // in the MAM window AND there are unseen *feed-visible* messages after
      // it, park the view on the first such message and render a divider
      // above it. ContentArea renders `feedMessages` (thread replies hidden),
      // so the anchor must match something actually rendered. Otherwise
      // scroll to bottom and track the newest feed message as last-seen.
      const lastSeenId = getLastSeen(roomKey(channelId));
      const lastSeenIdx = lastSeenId
        ? timelineWithQueue.findIndex((m) => matchMessageId(m, lastSeenId))
        : -1;
      const firstUnseen =
        lastSeenIdx !== -1 && lastSeenIdx < timelineWithQueue.length - 1
          ? timelineWithQueue.slice(lastSeenIdx + 1).find(isFeedVisible)
          : undefined;
      if (firstUnseen) {
        firstUnseenId.value = firstUnseen.id;
        await scrollFirstUnseenIntoView(firstUnseen.id);
      } else {
        firstUnseenId.value = null;
        await scrollToPinnedEdgeAndPin();
        const newest = [...timelineWithQueue].reverse().find(isFeedVisible);
        if (newest) persistLastSeen(channelId, newest.id);
      }
    } catch (e) {
      if (requestId === messageRequestId) {
        const queuedOnly = appendQueuedMessages([], roomJid);
        messages.value = queuedOnly;
        actionError.value = queuedOnly.length > 0 ? "" : normalizeError(e);
        isLoadingMessages.value = false;
      }
    }
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
    if (!client || !spaceId || !channelId || !threadId || !session.value) return;
    const results = await client.queryMamByThread(spaceId, channelId, threadId, 100);
    if (
      xmppClient.value !== client ||
      activeSpaceId.value !== spaceId ||
      activeChannelId.value !== channelId
    ) {
      return;
    }
    const appended: TimelineMessage[] = [];
    const localById = new Map<string, TimelineMessage>();
    for (const message of messages.value) {
      indexMessageByIds(localById, message);
    }
    for (const raw of results) {
      if (findMessageById(messages.value, raw.id)) continue;
      const tm = fromLiveMessage(session.value, raw, (id) => localById.get(id));
      indexMessageByIds(localById, tm);
      appended.push(tm);
    }
    if (appended.length === 0) return;
    messages.value = [...messages.value, ...appended].sort((a, b) =>
      a.createdAt.localeCompare(b.createdAt),
    );
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
    if (!client || !channelId) return;
    if (!bodyText.trim() && !hasFiles) return;
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
      const wireReplyTo = replyTo && parent
        ? {
            id: parent.id,
            author: parent.authorJid ?? replyTo.author,
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
        activeSpaceId.value === spaceId &&
        activeChannelId.value === channelId;

      if (msgId && session.value && isStillCurrentChannel) {
        // Optimistic insert: show message immediately with "sending" status
        const optimistic: TimelineMessage = {
          id: msgId,
          author: session.value.username,
          authorJid: `${currentRoomJid.value}/${session.value.username}`,
          body: bodyText || (attachments?.[0]?.url ?? ""),
          createdAt: new Date().toISOString(),
          isSelf: true,
          deliveryStatus: (result?.state ?? "sending") as DeliveryStatus,
          ...(markup && markup.length > 0 ? { markup } : {}),
          ...(references && references.length > 0 ? { references } : {}),
        };
        if (replyTo && parent) {
          optimistic.replyTo = {
            id: parent.id,
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
        void scrollToPinnedEdge();
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
    if (!xmppClient.value || !activeSpaceId.value || !activeChannelId.value || !session.value)
      return;

    // Compute the new reaction set for this user
    const msg = findMessageById(messages.value, messageId);
    const targetId = msg?.id ?? messageId;
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
        activeSpaceId.value,
        activeChannelId.value,
        targetId,
        [...myEmojis],
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activeSpaceId.value || !activeChannelId.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;

    clearActionError();

    try {
      await xmppClient.value.sendRetraction(
        activeSpaceId.value,
        activeChannelId.value,
        targetId,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function moderateMessage(messageId: string, reason?: string) {
    if (!xmppClient.value || !activeSpaceId.value || !activeChannelId.value) return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;

    clearActionError();

    try {
      await xmppClient.value.sendModeration(
        activeSpaceId.value,
        activeChannelId.value,
        targetId,
        reason,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function editMessage(messageId: string, newBody: string, markup?: MarkupSpan[], references?: MessageReference[]) {
    if (!xmppClient.value || !activeSpaceId.value || !activeChannelId.value || !newBody.trim())
      return;
    const targetId = findMessageById(messages.value, messageId)?.id ?? messageId;

    clearActionError();

    try {
      await xmppClient.value.sendCorrection(
        activeSpaceId.value,
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
    pendingEchoClientIds.clear();
    // $xmppStatus is authoritative and owned by XmppProvider; do not write it here.
    clearTypingState();
    isLoadingMessages.value = false;
    isSearching.value = false;
    firstUnseenId.value = null;
  }

  function clearMessages() {
    messageRequestId++;
    pendingEchoClientIds.clear();
    messages.value = [];
    isLoadingMessages.value = false;
    clearTypingState();
    firstUnseenId.value = null;
  }

  async function searchMessages(query: string) {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value;
    const channelId = activeChannelId.value;
    if (!client || !spaceId || !channelId) return;
    const requestId = ++searchRequestId;
    const trimmed = query.trim();
    searchQuery.value = trimmed;
    if (!trimmed) {
      searchResults.value = [];
      isSearching.value = false;
      return;
    }
    isSearching.value = true;
    try {
      const results = await client.searchMessages(spaceId, channelId, trimmed);
      if (
        requestId === searchRequestId &&
        xmppClient.value === client &&
        activeSpaceId.value === spaceId &&
        activeChannelId.value === channelId
      ) {
        searchResults.value = results;
      }
    } catch {
      if (requestId === searchRequestId) {
        searchResults.value = [];
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

  return {
    xmppStatus,
    messages,
    firstUnseenId,
    draft,
    forumPostTitle,
    isLoadingMessages,
    isSending,
    timelineEl,
    currentRoomJid,
    typingUsers,
    roomHats,
    roomPresence,
    roomLastSeen,
    slowModeCooldown,
    loadMessages,
    backfillThread,
    selectChannel,
    sendMessage,
    uploadProgress,
    editMessage,
    retractMessage,
    moderateMessage,
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
    lastMentionActivity,
    onMessageQueueStatus,
    onMessageAck,
    onMessageDeliveryFailure,
    onSessionLifecycle,
  };
}
