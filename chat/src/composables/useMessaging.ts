import { ref, computed, nextTick, type Ref } from "vue";
import type { WaddleApi, ChannelMessage } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  roomBareJidFor,
  type LiveRoomMessage,
  type XmppStatusSnapshot,
} from "@/lib/xmpp-client";
import type { TimelineMessage } from "@/lib/chat-ui";

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
  activeWaddleId: Ref<string | null>,
  activeChannelId: Ref<string | null>,
  normalizeError: (v: unknown) => string,
  actionError: Ref<string>,
  clearActionError: () => void,
) {
  let xmpp: BrowserXmppClient | null = null;
  const xmppStatus = ref<XmppStatusSnapshot>({
    state: "offline",
    detail: "Live room offline",
  });

  const messages = ref<TimelineMessage[]>([]);
  const draft = ref("");
  const isLoadingMessages = ref(false);
  const isSending = ref(false);
  const timelineEl: Ref<HTMLDivElement | null> = ref(null);

  let messageRequestId = 0;

  const currentRoomJid = computed(() => {
    if (!session.value || !activeWaddleId.value || !activeChannelId.value) return null;
    return roomBareJidFor(session.value, activeWaddleId.value, activeChannelId.value);
  });

  async function scrollToBottom() {
    await nextTick();
    timelineEl.value?.scrollTo({
      top: timelineEl.value.scrollHeight,
      behavior: "auto",
    });
  }

  function mergeLiveMessage(msg: TimelineMessage) {
    if (messages.value.some((m) => m.id === msg.id)) return;
    messages.value = [...messages.value, msg];
    void scrollToBottom();
  }

  async function ensureXmpp() {
    if (!session.value || xmpp) return;

    xmpp = new BrowserXmppClient(
      session.value,
      (msg) => {
        if (!currentRoomJid.value || msg.roomJid !== currentRoomJid.value || msg.type !== "message") return;
        mergeLiveMessage(fromLiveMessage(session.value!, msg));
      },
      (status) => {
        xmppStatus.value = status;
      },
    );

    try {
      await xmpp.connect();
    } catch (e) {
      xmppStatus.value = { state: "error", detail: normalizeError(e) };
    }
  }

  async function loadMessages(waddleId: string, channelId: string) {
    if (!api.value || !session.value) return;

    const requestId = ++messageRequestId;
    isLoadingMessages.value = true;
    clearActionError();

    try {
      await ensureXmpp();

      const [history] = await Promise.all([
        api.value.listMessages(waddleId, channelId),
        xmpp?.switchRoom(waddleId, channelId),
      ]);

      if (requestId !== messageRequestId || activeWaddleId.value !== waddleId || activeChannelId.value !== channelId) {
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
    await loadMessages(activeWaddleId.value, channelId);
  }

  async function sendMessage() {
    if (!xmpp || !activeWaddleId.value || !activeChannelId.value || !draft.value.trim()) return;

    isSending.value = true;
    clearActionError();

    try {
      await xmpp.sendGroupMessage(activeWaddleId.value, activeChannelId.value, draft.value);
      draft.value = "";
    } catch (e) {
      actionError.value = normalizeError(e);
    } finally {
      isSending.value = false;
    }
  }

  async function disconnect() {
    if (xmpp) {
      const ref = xmpp;
      xmpp = null;
      await ref.disconnect().catch(() => undefined);
    }
    xmppStatus.value = { state: "offline", detail: "Live room offline" };
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
    loadMessages,
    selectChannel,
    sendMessage,
    disconnect,
    clearMessages,
    scrollToBottom,
  };
}
