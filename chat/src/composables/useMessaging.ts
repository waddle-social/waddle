import { ref, computed, nextTick, watch, type Ref } from "vue";
import type { WaddleApi, ChannelMessage } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  roomBareJidFor,
  type LiveRoomMessage,
  type XmppStatusSnapshot,
  type ChatStateType,
} from "@/lib/xmpp-client";
import type { DeliveryStatus, TimelineMessage } from "@/lib/chat-ui";

function toTimelineMessage(session: WaddleSession, msg: ChannelMessage): TimelineMessage {
  return {
    id: msg.id,
    author: msg.author.display_name || msg.author.username,
    body: msg.content ?? "",
    createdAt: msg.created_at,
    isSelf: msg.author.user_id === session.user_id,
  };
}

function fromLiveMessage(session: WaddleSession, msg: LiveRoomMessage): TimelineMessage {
  return {
    id: msg.id,
    author: msg.nick,
    body: msg.body,
    createdAt: msg.createdAt,
    isSelf: msg.nick === session.username,
  };
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
  api: Ref<WaddleApi | null>,
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
    // Check if this is a self-echo confirming delivery of an optimistic message
    const existing = messages.value.find((m) => m.id === msg.id);
    if (existing) {
      if (existing.deliveryStatus === "sending") {
        // Self-echo received: upgrade from "sending" to "delivered"
        messages.value = messages.value.map((m) =>
          m.id === msg.id ? { ...m, deliveryStatus: "delivered" as DeliveryStatus } : m,
        );
      }
      return;
    }
    messages.value = [...messages.value, msg];
    void scrollToBottom();
  }

  async function loadMessages(waddleId: string, channelId: string) {
    if (!api.value || !session.value) return;

    const requestId = ++messageRequestId;
    isLoadingMessages.value = true;
    clearActionError();

    try {
      const [history] = await Promise.all([
        api.value.listMessages(waddleId, channelId),
        xmppClient.value?.switchRoom(waddleId, channelId),
      ]);

      if (
        requestId !== messageRequestId ||
        activeWaddleId.value !== waddleId ||
        activeChannelId.value !== channelId
      ) {
        return;
      }

      messages.value = history.messages.reverse().map((m) => toTimelineMessage(session.value!, m));
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

  async function sendMessage() {
    if (
      !xmppClient.value ||
      !activeWaddleId.value ||
      !activeChannelId.value ||
      !draft.value.trim()
    )
      return;

    isSending.value = true;
    clearActionError();

    const bodyText = draft.value;

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

      draft.value = "";
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

  return {
    xmppStatus,
    messages,
    draft,
    isLoadingMessages,
    isSending,
    timelineEl,
    currentRoomJid,
    typingUsers,
    loadMessages,
    selectChannel,
    sendMessage,
    editMessage,
    retractMessage,
    notifyComposing,
    disconnect,
    clearMessages,
    scrollToBottom,
  };
}
