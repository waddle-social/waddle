import { getCurrentScope, onScopeDispose, ref, type Ref } from "vue";
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

  // Defense-in-depth: parallels the channel-side composable. Orchestrator
  // explicitly calls clearTypingState() on disconnect / clearMessages, but
  // scope teardown without that contract being honoured shouldn't leak
  // timers either.
  if (getCurrentScope()) {
    onScopeDispose(() => clearTypingState());
  }

  function notifyComposing(thread?: { id: string; parent?: string }) {
    const client = xmppClient.value;
    const peerJid = activePeerJid.value;
    if (!client || !peerJid) return;
    const scope = conversationScope?.(peerJid) ?? "account";
    if (lastChatState !== "composing") {
      lastChatState = "composing";
      void client.sendDmChatState(peerJid, "composing", thread, scope).catch(() => undefined);
    }
    if (composingTimeout) clearTimeout(composingTimeout);
    composingTimeout = setTimeout(() => {
      if (xmppClient.value !== client || activePeerJid.value !== peerJid) return;
      lastChatState = "paused";
      void client.sendDmChatState(peerJid, "paused", thread, scope).catch(() => undefined);
    }, COMPOSING_PAUSE_MS);
  }

  /**
   * Internal-state reset called by useChatSend's onSendComplete after a
   * successful send: cancels any in-flight pause timer and snaps
   * `lastChatState` back to "active". Does NOT emit a wire `<active/>`
   * notification — the orchestrator sends
   * `client.sendDmChatState(peerJid, "active")` after this returns, per
   * XEP-0085 §4.2. The split exists so the composable's in-memory
   * debounce state stays consistent on every send while the orchestrator
   * decides (based on the wasm-client send result) whether the
   * wire-level `<active/>` actually fires.
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
    resetOnSend,
  };
}
