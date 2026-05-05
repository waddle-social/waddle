import { describe, test, expect, mock } from "bun:test";
import { effectScope, ref } from "vue";
import { useChatReadReceipts } from "../src/shell/read-receipts";

type Harness = {
  isWindowFocused: ReturnType<typeof ref<boolean>>;
  isPinnedAtEdge: ReturnType<typeof ref<boolean>>;
  activeKind: ReturnType<typeof ref<"channel" | "dm" | null>>;
  activeRoomJid: ReturnType<typeof ref<string | null>>;
  activePeerJid: ReturnType<typeof ref<string | null>>;
  latestRemoteMessageId: ReturnType<typeof ref<string | null>>;
  unreadCountForActive: ReturnType<typeof ref<number>>;
  markChannelRead: ReturnType<typeof mock>;
  markDmRead: ReturnType<typeof mock>;
  markDisplayed: ReturnType<typeof mock>;
  stop: () => void;
};

function makeHarness(initial: Partial<Pick<Harness,
  "activeKind" | "activeRoomJid" | "activePeerJid" |
  "latestRemoteMessageId" | "unreadCountForActive"
>> & { focused?: boolean; pinned?: boolean } = {}): Harness {
  const isWindowFocused = ref(initial.focused ?? true);
  const isPinnedAtEdge = ref(initial.pinned ?? true);
  const activeKind = ref<"channel" | "dm" | null>(null);
  const activeRoomJid = ref<string | null>(null);
  const activePeerJid = ref<string | null>(null);
  const latestRemoteMessageId = ref<string | null>(null);
  const unreadCountForActive = ref<number>(0);
  const markChannelRead = mock(() => undefined);
  const markDmRead = mock(() => undefined);
  const markDisplayed = mock(() => undefined);

  const scope = effectScope();
  scope.run(() => {
    useChatReadReceipts({
      isWindowFocused,
      isPinnedAtEdge,
      activeKind,
      activeRoomJid,
      activePeerJid,
      latestRemoteMessageId,
      unreadCountForActive,
      markChannelRead,
      markDmRead,
      markDisplayed,
    });
  });

  return {
    isWindowFocused,
    isPinnedAtEdge,
    activeKind,
    activeRoomJid,
    activePeerJid,
    latestRemoteMessageId,
    unreadCountForActive,
    markChannelRead,
    markDmRead,
    markDisplayed,
    stop: () => scope.stop(),
  };
}

describe("useChatReadReceipts", () => {
  test("does nothing when no active conversation", async () => {
    const h = makeHarness();
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 5;
    await Promise.resolve();
    expect(h.markDisplayed).not.toHaveBeenCalled();
    expect(h.markChannelRead).not.toHaveBeenCalled();
    expect(h.markDmRead).not.toHaveBeenCalled();
    h.stop();
  });

  test("dispatches markRead and markDisplayed for active channel when focused + pinned + unread", async () => {
    const h = makeHarness();
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 3;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledWith("m1");
    expect(h.markChannelRead).toHaveBeenCalledWith("room@chat.example.com");
    expect(h.markDmRead).not.toHaveBeenCalled();
    h.stop();
  });

  test("dispatches markRead and markDisplayed for active DM when focused + pinned + unread", async () => {
    const h = makeHarness();
    h.activeKind.value = "dm";
    h.activePeerJid.value = "bob@example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 1;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledWith("m1");
    expect(h.markDmRead).toHaveBeenCalledWith("bob@example.com");
    expect(h.markChannelRead).not.toHaveBeenCalled();
    h.stop();
  });

  test("does not dispatch when window is unfocused", async () => {
    const h = makeHarness({ focused: false });
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 3;
    await Promise.resolve();
    expect(h.markDisplayed).not.toHaveBeenCalled();
    expect(h.markChannelRead).not.toHaveBeenCalled();

    h.isWindowFocused.value = true;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledWith("m1");
    expect(h.markChannelRead).toHaveBeenCalledWith("room@chat.example.com");
    h.stop();
  });

  test("does not dispatch when not pinned at edge", async () => {
    const h = makeHarness({ pinned: false });
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 3;
    await Promise.resolve();
    expect(h.markChannelRead).not.toHaveBeenCalled();

    h.isPinnedAtEdge.value = true;
    await Promise.resolve();
    expect(h.markChannelRead).toHaveBeenCalledWith("room@chat.example.com");
    h.stop();
  });

  test("does not dispatch markDisplayed twice for the same id", async () => {
    const h = makeHarness();
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 1;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledTimes(1);

    h.unreadCountForActive.value = 0;
    await Promise.resolve();
    h.unreadCountForActive.value = 0;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("dispatches markDisplayed again when latestId advances", async () => {
    const h = makeHarness();
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 1;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledWith("m1");

    h.unreadCountForActive.value = 0;
    h.latestRemoteMessageId.value = "m2";
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledWith("m2");
    expect(h.markDisplayed).toHaveBeenCalledTimes(2);
    // unread is 0, so markRead should not have been called for the second event.
    expect(h.markChannelRead).toHaveBeenCalledTimes(1);
    h.stop();
  });

  test("resets dedup tracking when active conversation changes", async () => {
    const h = makeHarness();
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room-a@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 2;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledTimes(1);

    h.activeRoomJid.value = "room-b@chat.example.com";
    h.latestRemoteMessageId.value = "m1";
    h.unreadCountForActive.value = 4;
    await Promise.resolve();
    expect(h.markDisplayed).toHaveBeenCalledTimes(2);
    expect(h.markChannelRead).toHaveBeenLastCalledWith("room-b@chat.example.com");
    h.stop();
  });

  test("clears channel unread when latestId is null (e.g. reaction-only or backlog bump)", async () => {
    const h = makeHarness();
    h.activeKind.value = "channel";
    h.activeRoomJid.value = "room@chat.example.com";
    h.latestRemoteMessageId.value = null;
    h.unreadCountForActive.value = 1;
    await Promise.resolve();
    expect(h.markChannelRead).toHaveBeenCalledWith("room@chat.example.com");
    expect(h.markDisplayed).not.toHaveBeenCalled();
    h.stop();
  });

  test("clears DM unread when latestId is null", async () => {
    const h = makeHarness();
    h.activeKind.value = "dm";
    h.activePeerJid.value = "bob@example.com";
    h.latestRemoteMessageId.value = null;
    h.unreadCountForActive.value = 2;
    await Promise.resolve();
    expect(h.markDmRead).toHaveBeenCalledWith("bob@example.com");
    expect(h.markDisplayed).not.toHaveBeenCalled();
    h.stop();
  });
});
