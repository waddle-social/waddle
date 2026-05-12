import { ref, type Ref } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";

// XEP-0085 chat-state notifications for the channel side. Owns:
//   - the typing-users ref (which remote occupants are currently composing)
//   - per-nick auto-expire timers so a missed "paused" doesn't strand a
//     remote occupant in the typing indicator forever
//   - the local debounce machine: notifyComposing → optional outbound
//     "composing", then a 3-second timer that fires "paused" once the
//     user stops typing
//
// The "active" reset on send is owned by useMucSend's onSendComplete
// callback; this composable exposes `resetOnSend` so the orchestrator
// (or, later, useMucSend directly) can request the post-send cleanup
// without reaching into private state.

type UseChannelChatStatesDeps = {
  xmppClient: Ref<BrowserXmppClient | null>;
  activeSpaceId: Ref<string | null>;
  activeChannelId: Ref<string | null>;
};

const TYPING_EXPIRY_MS = 5000;
const COMPOSING_PAUSE_MS = 3000;

export function useChannelChatStates(deps: UseChannelChatStatesDeps) {
  const { xmppClient, activeSpaceId, activeChannelId } = deps;

  const typingUsers = ref<string[]>([]);
  const typingTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let lastChatState: "active" | "composing" | "paused" = "active";
  let composingTimeout: ReturnType<typeof setTimeout> | null = null;

  function addTypingUser(nick: string) {
    if (!typingUsers.value.includes(nick)) {
      typingUsers.value = [...typingUsers.value, nick];
    }
    // Auto-expire after TYPING_EXPIRY_MS in case a "paused" notification
    // is dropped — otherwise the indicator stays on indefinitely.
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

  /**
   * XEP-0085 §5.2 / §5.3 composing/paused debounce. Sends "composing"
   * the first time the user types in a window of activity, then arms a
   * 3-second timer to send "paused" once typing stops. Re-arms on every
   * call.
   */
  function notifyComposing() {
    const client = xmppClient.value;
    const spaceId = activeSpaceId.value ?? "";
    const channelId = activeChannelId.value;
    if (!client || !channelId) return;

    if (lastChatState !== "composing") {
      lastChatState = "composing";
      void client.sendChatState(spaceId, channelId, "composing").catch(() => undefined);
    }

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
    }, COMPOSING_PAUSE_MS);
  }

  /**
   * Called by useMucSend's onSendComplete after a successful send to
   * cancel any in-flight pause timer and snap the local state back to
   * "active" (XEP-0085 §4.2 — content messages SHOULD include
   * <active/>). The orchestrator routes this from useMucSend until that
   * composable starts holding a direct ref to useChannelChatStates.
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
