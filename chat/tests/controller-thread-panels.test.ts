import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useThreadPanels, type ActiveRightPanel } from "../src/shell/controllers/use-thread-panels";
import type { ExtensionRouteKey } from "../src/shell/controllers/use-extension-routes";
import { useMessageThreads } from "../src/channels/threads";
import type { TimelineMessage } from "../src/lib/chat-ui";
import type { ChannelSummary } from "../src/lib/xmpp-client";
import type { useWaddleDirectory } from "../src/waddles/directory";
import type { useChannelMessages } from "../src/channels/messages";
import type { useDirectMessages } from "../src/dms/messages";
import type { useDirectMessageConversations } from "../src/dms/conversations";
import type { useChannelInbox } from "../src/channels/inbox";

function makeHarness() {
  const ui = useChatShellState();
  ui.activePage.value = "chat";
  const activeChannelId = ref<string | null>("general");
  const currentChannel = ref<ChannelSummary | null>({ id: "general", name: "General" } as ChannelSummary);
  const activePeerJid = ref<string | null>(null);
  const activeThreadStack = ref<string[]>([]);
  const activeThreadTargetMessageId = ref<string | null>(null);
  const activeThreadTargetRequestId = ref(0);
  const activeRightPanel = ref<ActiveRightPanel | null>(null);
  const activeExtensionRouteKey = ref<ExtensionRouteKey | null>(null);
  const messages = ref<readonly TimelineMessage[]>([]);

  const channelBackfill = mock(async () => {});
  const dmBackfill = mock(async () => {});
  const markThreadRead = mock(() => {});
  const selectChannel = mock(async () => {});
  const openDm = mock(async () => {});
  const exitReactionMode = mock(() => {});
  const updateUrl = mock(() => {});

  const waddles = {
    activeChannelId,
    currentChannel,
    channels: ref<ChannelSummary[]>([]),
  } as unknown as ReturnType<typeof useWaddleDirectory>;
  const messaging = {
    backfillThread: channelBackfill,
  } as unknown as ReturnType<typeof useChannelMessages>;
  const dmMessaging = {
    backfillThread: dmBackfill,
  } as unknown as ReturnType<typeof useDirectMessages>;
  const dmConversations = {
    activePeerJid,
  } as unknown as ReturnType<typeof useDirectMessageConversations>;
  const channelUnread = {
    markThreadRead,
  } as unknown as ReturnType<typeof useChannelInbox>;

  const scope = effectScope();
  const panels = scope.run(() =>
    useThreadPanels({
      ui,
      waddles,
      messaging,
      dmMessaging,
      dmConversations,
      channelUnread,
      threads: useMessageThreads(messages),
      isActiveDirectDmSurface: () => ui.sidebarMode.value === "dms" && !!activePeerJid.value,
      activeChannelRoomJid: computed(() => "general@muc.example.com"),
      managedMucDomain: computed(() => "muc.example.com"),
      activeThreadStack,
      activeThreadTargetMessageId,
      activeThreadTargetRequestId,
      activeRightPanel,
      activeExtensionRouteKey,
      roomJidForChannelId: () => "general@muc.example.com",
      selectChannel,
      openDm,
      exitReactionMode,
      updateUrl,
    }),
  )!;

  return {
    ui,
    scope,
    panels,
    activeChannelId,
    activePeerJid,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeThreadTargetRequestId,
    activeRightPanel,
    activeExtensionRouteKey,
    messages,
    channelBackfill,
    dmBackfill,
    markThreadRead,
    selectChannel,
    openDm,
    exitReactionMode,
    updateUrl,
  };
}

describe("useThreadPanels thread stack", () => {
  test("openThread seeds the stack and backfills from the channel archive", () => {
    const h = makeHarness();
    h.panels.openThread("t1");

    expect(h.activeRightPanel.value).toBe("thread");
    expect(h.activeThreadStack.value).toEqual(["t1"]);
    expect(h.channelBackfill).toHaveBeenCalledWith("t1");
    expect(h.dmBackfill).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("re-opening the active thread with a target message re-backfills without resetting the stack", () => {
    const h = makeHarness();
    h.panels.openThread("t1");
    h.panels.pushThread("t2");
    expect(h.activeThreadStack.value).toEqual(["t1", "t2"]);

    const requestsBefore = h.activeThreadTargetRequestId.value;
    h.panels.openThread("t2", "m9");

    expect(h.activeThreadStack.value).toEqual(["t1", "t2"]);
    expect(h.activeThreadTargetMessageId.value).toBe("m9");
    expect(h.activeThreadTargetRequestId.value).toBe(requestsBefore + 1);
    expect(h.channelBackfill).toHaveBeenCalledTimes(3);
    h.scope.stop();
  });

  test("popThreadTo trims the breadcrumb and closes the panel below index 0", () => {
    const h = makeHarness();
    h.panels.openThread("t1");
    h.panels.pushThread("t2");
    h.panels.pushThread("t3");

    h.panels.popThreadTo(1);
    expect(h.activeThreadStack.value).toEqual(["t1", "t2"]);
    expect(h.activeRightPanel.value).toBe("thread");

    h.panels.popThreadTo(-1);
    expect(h.activeThreadStack.value).toEqual([]);
    expect(h.activeRightPanel.value).toBeNull();
    h.scope.stop();
  });

  test("DM surfaces backfill through the personal archive instead", () => {
    const h = makeHarness();
    h.ui.sidebarMode.value = "dms";
    h.activePeerJid.value = "bob@example.com";

    h.panels.openThread("dm-thread");
    expect(h.dmBackfill).toHaveBeenCalledWith("dm-thread");
    expect(h.channelBackfill).not.toHaveBeenCalled();
    h.scope.stop();
  });
});

describe("useThreadPanels right-panel arbitration", () => {
  test("closing the pinned panel falls back to the open thread", () => {
    const h = makeHarness();
    h.panels.openThread("t1");
    h.ui.showPinnedPanel.value = true;
    h.panels.activateRightPanel("pinned");
    expect(h.activeRightPanel.value).toBe("pinned");

    h.panels.closePinnedPanel();
    expect(h.ui.showPinnedPanel.value).toBe(false);
    expect(h.activeRightPanel.value).toBe("thread");
    h.scope.stop();
  });

  test("activateRightPanel refuses unavailable panels", () => {
    const h = makeHarness();
    // No pinned flag and no thread stack: nothing to activate.
    h.panels.activateRightPanel("pinned");
    expect(h.activeRightPanel.value).toBeNull();
    h.panels.activateRightPanel("thread");
    expect(h.activeRightPanel.value).toBeNull();
    h.scope.stop();
  });

  test("switching channels resets every per-conversation panel", async () => {
    const h = makeHarness();
    h.panels.openThread("t1");
    h.ui.showPinnedPanel.value = true;
    h.activeExtensionRouteKey.value = { channelId: "general", pluginId: "p", routeId: "r" };

    h.activeChannelId.value = "other";
    await nextTick();

    expect(h.activeThreadStack.value).toEqual([]);
    expect(h.activeThreadTargetMessageId.value).toBeNull();
    expect(h.ui.showPinnedPanel.value).toBe(false);
    expect(h.activeExtensionRouteKey.value).toBeNull();
    expect(h.activeRightPanel.value).toBeNull();
    expect(h.exitReactionMode).toHaveBeenCalled();
    expect(h.updateUrl).toHaveBeenCalled();
    h.scope.stop();
  });
});

describe("useThreadPanels thread selection", () => {
  test("onSelectThread reuses the active channel and marks the thread read", async () => {
    const h = makeHarness();
    await h.panels.onSelectThread("general", "t1");

    expect(h.selectChannel).not.toHaveBeenCalled();
    expect(h.markThreadRead).toHaveBeenCalledWith("general@muc.example.com", "t1");
    expect(h.activeThreadStack.value).toEqual(["t1"]);
    h.scope.stop();
  });

  test("onSelectThread navigates first when another channel owns the thread", async () => {
    const h = makeHarness();
    await h.panels.onSelectThread("other", "t2");

    expect(h.selectChannel).toHaveBeenCalledWith("other");
    expect(h.activeThreadStack.value).toEqual(["t2"]);
    h.scope.stop();
  });

  test("onSelectThreadEntry routes DM partners to the DM surface", async () => {
    const h = makeHarness();
    await h.panels.onSelectThreadEntry("bob@example.com", "dm-thread");

    expect(h.openDm).toHaveBeenCalledWith("bob@example.com");
    expect(h.activeThreadStack.value).toEqual(["dm-thread"]);
    h.scope.stop();
  });
});
