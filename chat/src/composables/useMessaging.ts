import { ref, computed, nextTick, watch, type Ref } from "vue";
import type { WaddleApi } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  roomBareJidFor,
  type LiveRoomMessage,
  type XmppStatusSnapshot,
  type ChatStateType,
  type RoomHats,
} from "@/lib/xmpp-client";
import type { DeliveryStatus, TimelineMessage } from "@/lib/chat-ui";

function fromLiveMessage(session: WaddleSession, msg: LiveRoomMessage): TimelineMessage {
  const tm: TimelineMessage = {
    id: msg.id,
    author: msg.nick,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: msg.nick === session.username,
  };
  if (msg.mentions && msg.mentions.length > 0) {
    tm.mentions = msg.mentions;
  }
  if (msg.sharedFile) {
    tm.sharedFile = msg.sharedFile;
  }
  if (msg.isSticker) {
    tm.isSticker = true;
  }
  if (msg.broadcastMention) {
    tm.broadcastMention = msg.broadcastMention;
  }
  if (msg.callInvite) {
    tm.callInvite = msg.callInvite;
  }
  return tm;
}

export function formatStamp(value: string) {
  return new Intl.DateTimeFormat("en", {
    hour: "2-digit",
    minute: "2-digit",
    month: "short",
    day: "numeric",
  }).format(new Date(value));
}

export function useMessaging(
  session: Ref<WaddleSession | null>,
  _api: Ref<WaddleApi | null>,
  xmppClient: Ref<BrowserXmppClient | null>,
  activeWaddleId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  const xmppStatus = ref<XmppStatusSnapshot>({
    state: "offline",
    detail: "Live room offline",
  });

  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const isLoadingMessages = ref(false);
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);
  const typingUsers = ref<string[]>([]);
  const roomHats = ref<RoomHats>({});
  const slowModeCooldown = ref(0);
  const activeChannels = ref<Set<string>>(new Set());
  const roomAvatarHashes = ref<Record<string, string>>({});
  const searchQuery = ref("");
  const searchResults = ref<{ id: string; nick: string; body: string; createdAt: string }[]>([]);
  const isSearching = ref(false);
  let slowModeTimer: ReturnType<typeof setInterval> | null = null;

  let messageRequestId = 0;
  let lastChatState: ChatStateType = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();

  const currentRoomJid = computed(() => {
    if (!session.value || !activeWaddleId.value || !activeChannelId.value) return null;
    return roomBareJidFor(session.value, activeWaddleId.value, activeChannelId.value);
  });

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
          applyCorrection(msg.replacesId, msg.body);
          return;
        }

        mergeLiveMessage(fromLiveMessage(session.value!, msg));
      });
      client.setStatusHandler((status) => {
        xmppStatus.value = status;
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
      client.setActivityHandler((roomJid) => {
        activeChannels.value = new Set([...activeChannels.value, roomJid]);
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
      xmppStatus.value = { state: "offline", detail: "Live room offline" };
      clearTypingState();
    }
  });

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
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;

    if (lastChatState !== "composing") {
      lastChatState = "composing";
      xmppClient.value.sendChatState(activeWaddleId.value, activeChannelId.value, "composing");
    }

    // Reset the pause timer: if user stops typing for 3s, send "paused"
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;
      lastChatState = "paused";
      xmppClient.value.sendChatState(activeWaddleId.value, activeChannelId.value, "paused");
    }, 3000);
  }

  async function scrollToBottom() {
    await nextTick();
    timelineEl.value?.scrollTo({
      top: timelineEl.value.scrollHeight,
      behavior: "auto",
    });
  }

  function applyDisplayed(messageId: string, nick: string) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.id !== messageId) return m;
      const existing = m.readBy ? [...m.readBy] : [];
      if (!existing.includes(nick)) {
        existing.push(nick);
      }
      return { ...m, readBy: existing };
    });
  }

  function markDisplayed(messageId: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;
    xmppClient.value.sendDisplayed(activeWaddleId.value, activeChannelId.value, messageId);
  }

  function applyReaction(messageId: string, nick: string, emojis: string[]) {
    messages.value = messages.value.map((m): TimelineMessage => {
      if (m.id !== messageId) return m;
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
      m.id === retractsId ? { ...m, body: "", isRetracted: true } : m,
    );
  }

  function applyCorrection(replacesId: string, newBody: string) {
    const idx = messages.value.findIndex((m) => m.id === replacesId);
    if (idx === -1) return;
    messages.value = messages.value.map((m) =>
      m.id === replacesId ? { ...m, body: newBody.trim(), isEdited: true } : m,
    );
  }

  function mergeLiveMessage(msg: TimelineMessage) {
    // Check if this is a self-echo confirming delivery of an optimistic message.
    // Match by ID first, then fall back to matching by body + author for cases
    // where the MUC server assigns a different stanza ID on reflection.
    const existing = messages.value.find(
      (m) =>
        m.id === msg.id ||
        (m.deliveryStatus === "sending" && m.isSelf && msg.isSelf && m.body === msg.body),
    );
    if (existing) {
      if (existing.deliveryStatus === "sending") {
        // Self-echo received: upgrade from "sending" to "delivered"
        messages.value = messages.value.map((m) =>
          m.id === existing.id ? { ...m, id: msg.id, deliveryStatus: "delivered" as DeliveryStatus } : m,
        );
      }
      return;
    }
    messages.value = [...messages.value, msg];
    void scrollToBottom();
  }

  async function loadMessages(waddleId: string, channelId: string) {
    if (!session.value) return;

    const requestId = ++messageRequestId;
    isLoadingMessages.value = true;
    clearActionError();

    try {
      // XEP-0313: Load message history via MAM (XMPP-native)
      const mamResults = xmppClient.value
        ? await xmppClient.value.queryMam(waddleId, channelId, 100)
        : [];

      if (
        requestId !== messageRequestId ||
        activeWaddleId.value !== waddleId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }

      // Separate regular messages from reaction updates
      const regularMessages: LiveRoomMessage[] = [];
      const reactionUpdates: { targetId: string; nick: string; emojis: string[] }[] = [];

      for (const msg of mamResults) {
        const reactionMsg = msg as LiveRoomMessage & { _reactionTarget?: string; _reactionEmojis?: string[] };
        if (reactionMsg._reactionTarget && reactionMsg._reactionEmojis) {
          reactionUpdates.push({
            targetId: reactionMsg._reactionTarget,
            nick: msg.nick,
            emojis: reactionMsg._reactionEmojis,
          });
        } else if (msg.body || msg.callInvite || msg.sharedFile) {
          regularMessages.push(msg);
        }
      }

      // Convert to timeline messages
      const timeline = regularMessages.map((m) => fromLiveMessage(session.value!, m));

      // Apply reactions from MAM history
      for (const update of reactionUpdates) {
        const target = timeline.find((m) => m.id === update.targetId);
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

      messages.value = timeline;
      await scrollToBottom();
    } catch (e) {
      if (requestId === messageRequestId) {
        actionError.value = normalizeError(e);
      }
    } finally {
      if (requestId === messageRequestId) {
        isLoadingMessages.value = false;
      }
    }
  }

  async function selectChannel(channelId: string) {
    if (!activeWaddleId.value) return;
    messages.value = [];
    clearTypingState();
    await loadMessages(activeWaddleId.value, channelId);
  }

  async function sendMessage(explicitBody?: string) {
    const bodyText = explicitBody ?? draft.value;
    if (
      !xmppClient.value ||
      !activeWaddleId.value ||
      !activeChannelId.value ||
      !bodyText.trim()
    )
      return;

    isSending.value = true;
    clearActionError();

    try {
      const msgId = await xmppClient.value.sendGroupMessage(
        activeWaddleId.value,
        activeChannelId.value,
        bodyText,
      );

      if (msgId && session.value) {
        // Optimistic insert: show message immediately with "sending" status
        const optimistic: TimelineMessage = {
          id: msgId,
          author: session.value.username,
          body: bodyText.trim(),
          createdAt: new Date().toISOString(),
          isSelf: true,
          deliveryStatus: "sending",
        };
        messages.value = [...messages.value, optimistic];
        void scrollToBottom();
      }

      // Only clear draft when sending from the composer (not explicit body)
      if (!explicitBody) {
        draft.value = "";
      }
      // Send "active" state after sending a message (stops composing indicator)
      if (composingTimeout) {
        clearTimeout(composingTimeout);
        composingTimeout = null;
      }
      lastChatState = "active";
      await xmppClient.value.sendChatState(activeWaddleId.value, activeChannelId.value, "active");
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
    }
  }

  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value || !session.value)
      return;

    // Compute the new reaction set for this user
    const msg = messages.value.find((m) => m.id === messageId);
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
        activeWaddleId.value,
        activeChannelId.value,
        messageId,
        [...myEmojis],
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;

    clearActionError();

    try {
      await xmppClient.value.sendRetraction(
        activeWaddleId.value,
        activeChannelId.value,
        messageId,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function moderateMessage(messageId: string, reason?: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;

    clearActionError();

    try {
      await xmppClient.value.sendModeration(
        activeWaddleId.value,
        activeChannelId.value,
        messageId,
        reason,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function editMessage(messageId: string, newBody: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value || !newBody.trim())
      return;

    clearActionError();

    try {
      await xmppClient.value.sendCorrection(
        activeWaddleId.value,
        activeChannelId.value,
        newBody,
        messageId,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  function disconnect() {
    xmppStatus.value = { state: "offline", detail: "Live room offline" };
    clearTypingState();
  }

  function clearMessages() {
    messages.value = [];
  }

  async function searchMessages(query: string) {
    if (!xmppClient.value || !activeWaddleId.value || !activeChannelId.value) return;
    const trimmed = query.trim();
    searchQuery.value = trimmed;
    if (!trimmed) {
      searchResults.value = [];
      return;
    }
    isSearching.value = true;
    try {
      searchResults.value = await xmppClient.value.searchMessages(
        activeWaddleId.value,
        activeChannelId.value,
        trimmed,
      );
    } catch {
      searchResults.value = [];
    } finally {
      isSearching.value = false;
    }
  }

  function clearSearch() {
    searchQuery.value = "";
    searchResults.value = [];
  }

  function clearChannelActivity(roomJid: string) {
    const next = new Set(activeChannels.value);
    next.delete(roomJid);
    activeChannels.value = next;
  }

  return {
    xmppStatus,
    messages,
    draft,
    isLoadingMessages,
    isSending,
    timelineEl,
    currentRoomJid,
    typingUsers,
    roomHats,
    slowModeCooldown,
    loadMessages,
    selectChannel,
    sendMessage,
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
    scrollToBottom,
  };
}
