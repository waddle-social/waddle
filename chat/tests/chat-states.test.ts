// Direct unit tests for useChannelChatStates and useDmChatStates covering
// the XEP-0085 typing-indicator + debounce machine.

import { describe, expect, mock, test } from "bun:test";
import { ref } from "vue";
import { useChannelChatStates } from "../src/channels/chat-states";
import { useDmChatStates } from "../src/dms/chat-states";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";

function makeChannelClient(sendChatState = mock(async () => undefined)): BrowserXmppClient {
  return { sendChatState } as unknown as BrowserXmppClient;
}

function makeDmClient(sendDmChatState = mock(async () => undefined)): BrowserXmppClient {
  return { sendDmChatState } as unknown as BrowserXmppClient;
}

describe("useChannelChatStates — typing indicator", () => {
  test("addTypingUser inserts a nick and dedups on repeated calls", () => {
    const states = useChannelChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient()),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
    });
    states.addTypingUser("alice");
    states.addTypingUser("alice");
    states.addTypingUser("bob");
    expect(states.typingUsers.value).toEqual(["alice", "bob"]);
  });

  test("removeTypingUser drops the nick", () => {
    const states = useChannelChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient()),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
    });
    states.addTypingUser("alice");
    states.removeTypingUser("alice");
    expect(states.typingUsers.value).toEqual([]);
  });

  test("clearTypingState wipes all typing nicks", () => {
    const states = useChannelChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient()),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
    });
    states.addTypingUser("alice");
    states.addTypingUser("bob");
    states.clearTypingState();
    expect(states.typingUsers.value).toEqual([]);
  });
});

describe("useChannelChatStates — outbound composing/paused (XEP-0085)", () => {
  test("notifyComposing sends composing on the first call only", () => {
    const sendChatState = mock(async () => undefined);
    const states = useChannelChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendChatState)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
    });
    states.notifyComposing();
    states.notifyComposing();
    // First call sends "composing"; the second is suppressed because
    // lastChatState is already "composing".
    const composingCalls = sendChatState.mock.calls.filter(
      (c) => c[2] === "composing",
    );
    expect(composingCalls.length).toBe(1);
  });

  test("notifyComposing is a no-op when no client / no channel", () => {
    const sendChatState = mock(async () => undefined);
    const states = useChannelChatStates({
      xmppClient: ref<BrowserXmppClient | null>(null),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
    });
    states.notifyComposing();
    expect(sendChatState).not.toHaveBeenCalled();
  });

  test("resetOnSend after notifyComposing lets a subsequent notifyComposing fire composing again", () => {
    const sendChatState = mock(async () => undefined);
    const states = useChannelChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeChannelClient(sendChatState)),
      activeSpaceId: ref("space"),
      activeChannelId: ref("general"),
    });
    states.notifyComposing();
    states.resetOnSend();
    states.notifyComposing();
    // resetOnSend snapped lastChatState back to "active", so the second
    // notifyComposing emits "composing" again.
    const composingCalls = sendChatState.mock.calls.filter(
      (c) => c[2] === "composing",
    );
    expect(composingCalls.length).toBe(2);
  });
});

describe("useDmChatStates", () => {
  test("notifyComposing sends DM chat-state to the active peer", () => {
    const sendDmChatState = mock(async () => undefined);
    const states = useDmChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmChatState)),
      activePeerJid: ref("bob@example.com"),
    });
    states.notifyComposing();
    expect(sendDmChatState).toHaveBeenCalledTimes(1);
    expect(sendDmChatState.mock.calls[0]![0]).toBe("bob@example.com");
    expect(sendDmChatState.mock.calls[0]![1]).toBe("composing");
  });

  test("addTypingUser + clearTypingState surfaces and clears typingUsers", () => {
    const states = useDmChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient()),
      activePeerJid: ref("bob@example.com"),
    });
    states.addTypingUser("bob");
    expect(states.typingUsers.value).toContain("bob");
    states.clearTypingState();
    expect(states.typingUsers.value).toEqual([]);
  });

  test("no-op without peerJid", () => {
    const sendDmChatState = mock(async () => undefined);
    const states = useDmChatStates({
      xmppClient: ref<BrowserXmppClient | null>(makeDmClient(sendDmChatState)),
      activePeerJid: ref<string | null>(null),
    });
    states.notifyComposing();
    expect(sendDmChatState).not.toHaveBeenCalled();
  });
});
