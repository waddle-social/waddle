import { describe, expect, mock, test } from "bun:test";
import { computed, effectScope, ref } from "vue";
import { useChatShellState } from "../src/shell/state";
import {
  applyMatchToShellState,
  useRouteSync,
} from "../src/shell/controllers/use-route-sync";
import type { ActiveRightPanel } from "../src/shell/controllers/use-thread-panels";
import type { ExtensionRouteKey } from "../src/shell/controllers/use-extension-routes";
import type { RouteMatch } from "../src/router";
import type { WaddleSession } from "../src/lib/server-auth";
import type { useWaddleDirectory } from "../src/waddles/directory";
import type { useChannelMessages } from "../src/channels/messages";
import type { useDirectMessages } from "../src/dms/messages";
import type { useDirectMessageConversations } from "../src/dms/conversations";

function session(): WaddleSession {
  return {
    username: "alice",
    jid: "alice@example.com",
    session_id: "s1",
    user_id: "u1",
    avatar_url: null,
    xmpp_localpart: "alice",
    xmpp_websocket_url: "wss://example.com/xmpp",
    is_expired: false,
    expires_at: null,
  } as WaddleSession;
}

describe("applyMatchToShellState", () => {
  test("maps every top-level route id onto the page ladder", () => {
    const ui = useChatShellState();

    applyMatchToShellState(ui, { id: "threads" } as RouteMatch);
    expect(ui.activePage.value).toBe("threads");

    applyMatchToShellState(ui, { id: "unread" } as RouteMatch);
    expect(ui.activePage.value).toBe("unread");

    applyMatchToShellState(ui, { id: "home" } as RouteMatch);
    expect(ui.activePage.value).toBe("dashboard");
    expect(ui.sidebarMode.value).toBe("channels");
    expect(ui.showPinnedPanel.value).toBe(false);
  });

  test("dm routes switch to the DM sidebar and carry the pinned flag", () => {
    const ui = useChatShellState();
    applyMatchToShellState(ui, {
      id: "dm",
      params: { username: "bob" },
      search: { thread: ["t1"], pinned: true },
    } as RouteMatch);

    expect(ui.activePage.value).toBe("chat");
    expect(ui.sidebarMode.value).toBe("dms");
    expect(ui.showPinnedPanel.value).toBe(true);
  });

  test("stories resolves to the feed surface with story composer intent", () => {
    const ui = useChatShellState();
    applyMatchToShellState(ui, { id: "stories" } as RouteMatch);

    expect(ui.activePage.value).toBe("chat");
    expect(ui.activeCommunitySurface.value).toBe("feed");
    expect(ui.feedDefaultFilter.value).toBe("stories");
    expect(ui.feedDefaultComposerMode.value).toBe("story");

    applyMatchToShellState(ui, { id: "feed" } as RouteMatch);
    expect(ui.feedDefaultFilter.value).toBe("all");
    expect(ui.feedDefaultComposerMode.value).toBe("post");
  });

  test("channel routes clear the pinned flag when the search says so", () => {
    const ui = useChatShellState();
    ui.showPinnedPanel.value = true;
    applyMatchToShellState(ui, {
      id: "channel",
      params: { channelId: "general" },
      search: { thread: [], pinned: false },
    } as RouteMatch);

    expect(ui.activePage.value).toBe("chat");
    expect(ui.sidebarMode.value).toBe("channels");
    expect(ui.showPinnedPanel.value).toBe(false);
  });
});

function makeHarness(overrides: { openDm?: (peerJid: string) => Promise<void> } = {}) {
  const ui = useChatShellState();
  const activeThreadStack = ref<string[]>([]);
  const activeThreadTargetMessageId = ref<string | null>(null);
  const activeRightPanel = ref<ActiveRightPanel | null>(null);
  const activeExtensionRouteKey = ref<ExtensionRouteKey | null>(null);
  const isApplyingRoute = ref(false);
  const activeChannelId = ref<string | null>("general");
  const channels = ref([
    { id: "general", name: "General", spaceId: "space-1" },
  ]);

  const closeDm = mock(() => {});
  const clearMessages = mock(() => {});
  const loadMessages = mock(async () => {});
  const channelBackfill = mock(async () => {});
  const dmBackfill = mock(async () => {});
  const reloadChannelMembers = mock(async () => {});
  const openDm = mock(overrides.openDm ?? (async () => {}));
  const selectGroupDm = mock(async () => true);
  const clearPendingChannelRoomJidSelection = mock(() => {});
  const clearPendingChannelRoute = mock(() => {});

  const waddles = {
    activeChannelId,
    channels,
    reloadChannelMembers,
  } as unknown as ReturnType<typeof useWaddleDirectory>;
  const messaging = {
    clearMessages,
    loadMessages,
    backfillThread: channelBackfill,
  } as unknown as ReturnType<typeof useChannelMessages>;
  const dmMessaging = {
    backfillThread: dmBackfill,
  } as unknown as ReturnType<typeof useDirectMessages>;
  const dmConversations = {
    closeDm,
  } as unknown as ReturnType<typeof useDirectMessageConversations>;

  const scope = effectScope();
  const routeSync = scope.run(() =>
    useRouteSync({
      ui,
      session: computed(() => session()),
      waddles,
      messaging,
      dmMessaging,
      dmConversations,
      isApplyingRoute,
      activeDmPeer: computed(() => null),
      activeThreadStack,
      activeThreadTargetMessageId,
      activeRightPanel,
      activeExtensionRouteKey,
      clearPendingChannelRoomJidSelection,
      openDm,
      selectGroupDm,
      clearPendingChannelRoute,
    }),
  )!;

  return {
    ui,
    scope,
    routeSync,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeRightPanel,
    activeExtensionRouteKey,
    activeChannelId,
    closeDm,
    clearMessages,
    loadMessages,
    channelBackfill,
    dmBackfill,
    reloadChannelMembers,
    openDm,
    clearPendingChannelRoomJidSelection,
  };
}

describe("useRouteSync applyRouteTarget", () => {
  test("home route drops the whole chat context", async () => {
    const h = makeHarness();
    h.activeChannelId.value = "general";
    h.activeThreadStack.value = ["t1"];
    h.activeRightPanel.value = "thread";
    h.activeExtensionRouteKey.value = { channelId: "general", pluginId: "p", routeId: "r" };

    await h.routeSync.applyRouteTarget({ id: "home" } as RouteMatch, h.routeSync.beginRouteRequest());

    expect(h.closeDm).toHaveBeenCalledTimes(1);
    expect(h.activeChannelId.value).toBeNull();
    expect(h.activeExtensionRouteKey.value).toBeNull();
    expect(h.activeRightPanel.value).toBeNull();
    expect(h.activeThreadStack.value).toEqual([]);
    expect(h.clearMessages).toHaveBeenCalledTimes(1);
    expect(h.clearPendingChannelRoomJidSelection).toHaveBeenCalledTimes(1);
    h.scope.stop();
  });

  test("dm route opens the peer on the session domain and restores panels", async () => {
    const h = makeHarness();
    await h.routeSync.applyRouteTarget({
      id: "dm",
      params: { username: "@bob" },
      search: { thread: ["t1", "t2", "t1"], pinned: false },
    } as RouteMatch, h.routeSync.beginRouteRequest());

    expect(h.openDm).toHaveBeenCalledWith("bob@example.com");
    expect(h.activeThreadStack.value).toEqual(["t1", "t2", "t1"]);
    expect(h.activeRightPanel.value).toBe("thread");
    // Dedup: the stack repeats t1 but each thread backfills once.
    expect(h.dmBackfill).toHaveBeenCalledTimes(2);
    h.scope.stop();
  });

  test("dm route with pinned search activates the pinned panel", async () => {
    const h = makeHarness();
    await h.routeSync.applyRouteTarget({
      id: "dm",
      params: { username: "bob" },
      search: { thread: [], pinned: true },
    } as RouteMatch, h.routeSync.beginRouteRequest());

    expect(h.ui.showPinnedPanel.value).toBe(true);
    expect(h.activeRightPanel.value).toBe("pinned");
    h.scope.stop();
  });

  test("a superseded route request stops mutating state after its await", async () => {
    let routeSyncRef: ReturnType<typeof makeHarness> | null = null;
    const h = makeHarness({
      openDm: async () => {
        // A newer navigation lands while the DM open is in flight.
        routeSyncRef!.routeSync.beginRouteRequest();
      },
    });
    routeSyncRef = h;
    h.activeThreadStack.value = ["existing"];

    await h.routeSync.applyRouteTarget({
      id: "dm",
      params: { username: "bob" },
      search: { thread: ["t9"], pinned: false },
    } as RouteMatch, h.routeSync.beginRouteRequest());

    // The stale request must not restore its thread stack.
    expect(h.activeThreadStack.value).toEqual(["existing"]);
    expect(h.dmBackfill).not.toHaveBeenCalled();
    h.scope.stop();
  });

  test("channel route activates the channel and loads its messages", async () => {
    const h = makeHarness();
    h.activeChannelId.value = null;

    await h.routeSync.applyRouteTarget({
      id: "channel",
      params: { channelId: "general" },
      search: { thread: ["t1"], pinned: false },
    } as RouteMatch, h.routeSync.beginRouteRequest());

    expect(h.activeChannelId.value).toBe("general");
    expect(h.reloadChannelMembers).toHaveBeenCalledWith("general");
    expect(h.clearMessages).toHaveBeenCalledTimes(1);
    expect(h.loadMessages).toHaveBeenCalledWith(
      "space-1",
      "general",
      0,
      [],
      { intent: "automatic" },
    );
    expect(h.activeThreadStack.value).toEqual(["t1"]);
    expect(h.activeRightPanel.value).toBe("thread");
    expect(h.channelBackfill).toHaveBeenCalledWith("t1");
    h.scope.stop();
  });

  test("explicit history navigation may retry a denied channel", async () => {
    const h = makeHarness();

    await h.routeSync.applyRouteTarget({
      id: "channel",
      params: { channelId: "general" },
      search: { thread: [], pinned: false },
    } as RouteMatch, h.routeSync.beginRouteRequest(), {
      intent: "explicit-navigation",
    });

    expect(h.loadMessages).toHaveBeenCalledWith(
      "space-1",
      "general",
      0,
      [],
      { intent: "explicit-navigation" },
    );
    h.scope.stop();
  });

  test("unknown channel slug clears the selection instead of guessing", async () => {
    const h = makeHarness();
    await h.routeSync.applyRouteTarget({
      id: "channel",
      params: { channelId: "missing" },
      search: { thread: [], pinned: false },
    } as RouteMatch, h.routeSync.beginRouteRequest());

    expect(h.activeChannelId.value).toBeNull();
    expect(h.clearMessages).toHaveBeenCalledTimes(1);
    expect(h.loadMessages).not.toHaveBeenCalled();
    h.scope.stop();
  });
});
