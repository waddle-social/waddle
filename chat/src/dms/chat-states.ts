import { getCurrentScope, onScopeDispose, ref, type Ref } from "vue";
import { useChatStatePreference } from "@/preferences/chat-states";
import type { BrowserXmppClient, DmConversationScope } from "@/lib/xmpp-client";

// XEP-0085 chat-state notifications for the DM side. Mirrors
// useChannelChatStates with the same debounce machine, scoped to the
// active peer JID instead of (spaceId, channelId).

type UseDmChatStatesDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  conversationScope?: (peerJid: string) => DmConversationScope;
};

const TYPING_EXPIRY_MS = 5000;
const COMPOSING_PAUSE_MS = 3000;

export function useDmChatStates(deps: UseDmChatStatesDeps) {
  const { xmppClient, activePeerJid, conversationScope } = deps;

  // XEP-0085 §5.1: users must be able to disable chat state notifications.
  // When disabled, nothing is sent (composing, paused, or active).
  const { sendChatStates } = useChatStatePreference();

  const typingUsers = ref<string[]>([]);
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  // XEP-0085 §5.1: chat states are only sent to a peer that has shown it
  // supports them by sending one. Our content messages carry <active/>, so
  // a supporting peer answers and this set fills; a peer that never does
  // is never sent composing/paused (implicit negotiation).
  const peersWithChatStates = new Set<string>();
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

  // Defense-in-depth: parallels the channel-side composable. Orchestrator
  // explicitly calls clearTypingState() on disconnect / clearMessages, but
  // scope teardown without that contract being honoured shouldn't leak
  // timers either.
  if (getCurrentScope()) {
    onScopeDispose(() => clearTypingState());
  }

  function notePeerChatState(peerJid: string) {
    peersWithChatStates.add(peerJid);
  }

  function notifyComposing(thread?: { id: string; parent?: string }) {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid || !sendChatStates.value) return;
    if (!peersWithChatStates.has(peerJid)) return;
    const scope = conversationScope?.(peerJid) ?? "account";
    if (lastChatState !== "composing") {
      lastChatState = "composing";
      void client.sendDmChatState(peerJid, "composing", thread, scope).catch(() => undefined);
    }
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (xmppClient.value !== client || activePeerJid.value !== peerJid) return;
      if (!sendChatStates.value) return;
      lastChatState = "paused";
      void client.sendDmChatState(peerJid, "paused", thread, scope).catch(() => undefined);
    }, COMPOSING_PAUSE_MS);
  }

  /**
   * Internal-state reset called by useChatSend's onSendComplete after a
   * send: cancels any in-flight pause timer and snaps `lastChatState`
   * back to "active". Emits nothing on the wire: the content message
   * already carries `<active/>` (XEP-0085 §5.1).
   */
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
    notePeerChatState,
    resetOnSend,
  };
}
