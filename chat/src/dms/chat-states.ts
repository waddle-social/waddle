import { ref, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";

// XEP-0085 chat-state notifications for the DM side. Mirrors
// useChannelChatStates with the same debounce machine, scoped to the
// active peer JID instead of (spaceId, channelId).

type UseDmChatStatesDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
};

const TYPING_EXPIRY_MS = 5000;
const COMPOSING_PAUSE_MS = 3000;

export function useDmChatStates(deps: UseDmChatStatesDeps) {
  const { xmppClient, activePeerJid } = deps;

  const typingUsers = ref<string[]>([]);
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let lastChatState: "active" | "composing" | "paused" = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;

  function addTypingUser(nick: string) {
    if (!typingUsers.value.includes(nick)) {
      typingUsers.value = [...typingUsers.value, nick];
    }
    const existing = typingTimers.get(nick);
    if (existing) clearTimeout(existing);
    typingTimers.set(
      nick,
      setTimeout(() => removeTypingUser(nick), TYPING_EXPIRY_MS),
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
    }, COMPOSING_PAUSE_MS);
  }

  function resetOnSend() {
    if (composingTimeout) {
      clearTimeout(composingTimeout);
      composingTimeout = null;
    }
    lastChatState = "active";
  }

  return {
    typingUsers,
    addTypingUser,
    removeTypingUser,
    clearTypingState,
    notifyComposing,
    resetOnSend,
  };
}
