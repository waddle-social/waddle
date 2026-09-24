import { afterEach, describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref, shallowReactive } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useWaddleDirectory } from "../src/waddles/directory";
import { usePageNavigation } from "../src/shell/controllers/use-page-navigation";
import { applyMatchToShellState, useRouteSync } from "../src/shell/controllers/use-route-sync";
import { useConnectionLifecycle } from "../src/shell/controllers/use-connection-lifecycle";
import { useDmSync } from "../src/shell/controllers/use-dm-sync";
import { useRoomSync } from "../src/shell/controllers/use-room-sync";
import { useThreadPanels } from "../src/shell/controllers/use-thread-panels";
import { useActiveConversation } from "../src/shell/controllers/use-active-conversation";
import { useSendOrchestration } from "../src/shell/controllers/use-send-orchestration";
import { useDirectMessageConversations } from "../src/dms/conversations";
import { buildHref, matchLocation } from "../src/router";
import type { UserSearchResult } from "../src/lib/chat-types";
import type { BrowserXmppClient } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";

const topology = {
  spaces: [],
  rooms: [{ id: "general", name: "General", jid: "general@muc.example.com", channelType: "text" as const }],
};
const noop = () => {};
const done = async () => {};
const originalWindow = (globalThis as Record<string, unknown>).window;
const scopes: ReturnType<typeof effectScope>[] = [];

afterEach(() => {
  for (const scope of scopes.splice(0)) scope.stop();
  if (originalWindow === undefined) delete (globalThis as Record<string, unknown>).window;
  else (globalThis as Record<string, unknown>).window = originalWindow;
});

async function flush() {
  for (let i = 0; i < 30; i++) await nextTick();
}

function harness(initialPath = "/r/general", discoveredTopology = topology) {
  const initialUrl = new URL(initialPath, "https://example.com");
  const location = { pathname: initialUrl.pathname, search: initialUrl.search };
  function move(_state: unknown, _title: string, href: string) {
    const url = new URL(href, "https://example.com");
    location.pathname = url.pathname;
    location.search = url.search;
  }
  const historyEntries = [location.pathname + location.search];
  let historyIndex = 0;
  const history = {
    pushState(state: unknown, title: string, href: string) {
      historyEntries.splice(historyIndex + 1);
      historyEntries.push(href);
      historyIndex++;
      move(state, title, href);
    },
    replaceState(state: unknown, title: string, href: string) {
      historyEntries[historyIndex] = href;
      move(state, title, href);
    },
    back() {
      if (historyIndex > 0) move(null, "", historyEntries[--historyIndex]!);
    },
  };
  (globalThis as Record<string, unknown>).window = {
    location,
    history,
    addEventListener: noop,
    removeEventListener: noop,
    sessionStorage: { getItem: () => null, setItem: noop },
  };

  const discoverTopology = mock(async () => discoveredTopology);
  const subscribeToPeerPresence = mock(done);
  const searchUsers = mock(async (_query: string): Promise<UserSearchResult[]> => [
    { id: "bob@example.com", jid: "bob@example.com", username: "bob", display_name: null, avatar_url: null },
  ]);
  const client = {
    discoverTopology,
    searchUsers,
    listRoomMembers: async () => [],
    subscribeToPeerPresence,
    rememberRoomJidForChannel: noop,
    setDirectMessageHandler: noop,
    setDmChatStateHandler: noop,
    setDmDisplayedHandler: noop,
    setDmReactionHandler: noop,
    setMdsDisplayedHandler: noop,
    setPresenceUpdateHandler: noop,
    addPubsubEventHandler: noop,
    setMemberJidHandler: noop,
    setMessageAckHandler: noop,
    setMessageDeliveryFailureHandler: noop,
    setQueuedMessageStatusHandler: noop,
    setInboxPushHandler: noop,
    setCatchupFailureHandler: noop,
    setSessionLifecycleHandler: noop,
  } as unknown as BrowserXmppClient;
  const session = computed(() => ({ jid: "alice@example.com", username: "alice" }) as WaddleSession);
  const xmppClient = computed(() => client);
  const connectionStore = shallowReactive({ appState: "loading", session: session.value });
  const scope = effectScope();
  scopes.push(scope);

  return scope.run(() => {
    const ui = useChatShellState();
    applyMatchToShellState(ui, matchLocation(location.pathname, location.search));
    const waddles = useWaddleDirectory(xmppClient, session, String, ui.actionError, ui.clearActionError);
    const isApplyingRoute = ref(false);
    const refreshExtensionRoutes = mock(done);
    const xmppStatus = ref({ state: "offline" });
    const activeThreadStack = ref<string[]>([]);
    const activeRightPanel = ref<string | null>(null);
    const messaging = {
      xmppStatus, loadMessages: mock(done), clearMessages: noop, backfillThread: mock(done),
      rememberChannelRoomJid: noop, clearChannelActivity: noop,
      sendMessage: mock(done), timelineEl: ref(null), timelineEdgeScroller: ref(null),
    };
    const dmMessaging = {
      loadMessages: mock(done), clearMessages: noop, backfillThread: mock(done),
      sendMessage: mock(done), timelineEl: ref(null), timelineEdgeScroller: ref(null),
    };
    const dmConversations = useDirectMessageConversations(
      session, xmppClient,
      computed(() => waddles.channels.value.flatMap((channel) => channel.jid ? [channel.jid] : [])),
    );
    const shared = {
      ui, waddles, session, xmppClient, isApplyingRoute, activeThreadStack, activeRightPanel,
      activeThreadTargetMessageId: ref(null), activeThreadTargetRequestId: ref(0),
      activeExtensionRouteKey: ref(null), memberJidByNick: ref({}),
      activeDmPeer: computed(() => dmConversations.conversations.value.find((peer) => peer.peerJid === dmConversations.activePeerJid.value) ?? null),
      dmConversations, dmMessaging, messaging,
    };
    const conversation = useActiveConversation(shared as never);
    const send = useSendOrchestration({ ...shared, ...conversation } as never);
    let lifecycle: ReturnType<typeof useConnectionLifecycle>;
    let routeSync: ReturnType<typeof useRouteSync>;
    const updateUrl = () => routeSync?.updateUrl();
    const cancelPendingRoute = () => routeSync?.cancelPendingRoute();
    const managedMucDomain = computed(() => "muc.example.com");
    const roomSync = useRoomSync({
      ...shared, managedMucDomain, computedChannelUnreadMap: computed(() => ({})), updateUrl, cancelPendingRoute,
    } as never);
    const clearPendingChannelRoomJidSelection = roomSync.clearPendingChannelRoomJidSelection;
    const dmSync = useDmSync({
      ...shared, selfDomain: computed(() => "example.com"), rosterContacts: { contacts: ref([]) },
      clearPendingChannelRoomJidSelection, cancelPendingRoute, updateUrl,
      selectGroupDm: roomSync.selectGroupDm,
    } as never);
    useThreadPanels({
      ...shared, ...roomSync, managedMucDomain, isActiveDirectDmSurface: () => !!dmConversations.activePeerJid.value,
      channelUnread: {}, threads: {}, openDm: dmSync.handleOpenDm, exitReactionMode: noop, updateUrl,
    } as never);
    routeSync = useRouteSync({
      ...shared, openDm: dmSync.handleOpenDm, selectGroupDm: roomSync.selectGroupDm, clearPendingChannelRoomJidSelection,
      clearPendingChannelRoute: () => lifecycle?.clearPendingChannelRoute(),
    } as never);
    const page = usePageNavigation({
      ...shared, updateUrl, cancelPendingRoute, clearPendingChannelRoomJidSelection,
    } as never);
    lifecycle = useConnectionLifecycle({
      ...shared, connectionStore, routeSync, clearPendingChannelRoomJidSelection,
      selectedChannelRoomJids: ref({}), extensionRoutes: ref([]), memberJidByNick: ref({}),
      isActiveDirectDmSurface: () => false,
      channelUnread: {}, rosterContacts: { loadRosterContacts: done },
      socialFeed: {}, stories: {}, communityEvents: {},
      notifications: { registerServiceWorker: done }, notifySettings: {},
      appUpdate: { start: noop, stop: noop }, presence: {},
      notificationOrchestration: { setupPushSubscription: done },
      refreshExtensionRoutes, showFirstRunSetupIfNeeded: noop, resetSetupPrompt: noop,
    } as never);
    return { ...shared, conversation, send, page, routeSync, dmSync, roomSync, connectionStore, discoverTopology, searchUsers, discoveredTopology, refreshExtensionRoutes, subscribeToPeerPresence, location, history, historyEntries, xmppStatus };
  })!;
}

function delayDiscovery(h: ReturnType<typeof harness>) {
  let resolve!: (value: typeof topology) => void;
  const discovery = new Promise<typeof topology>((done) => { resolve = done; });
  h.discoverTopology.mockImplementation(() => discovery);
  return () => resolve(h.discoveredTopology);
}

describe("navigation during connection discovery", () => {
  test.each([false, true])("Back to a removed group clears panels and cannot send to the previous channel (pinned: %s)", async (pinned) => {
    const group = { id: "crew", name: "Crew", jid: "crew@muc.example.com", channelType: "text" as const, isGroupDm: true };
    const initialPath = buildHref({
      id: "groupDmRoom", params: { roomJid: group.jid }, search: { thread: ["group-thread"], pinned },
    });
    const h = harness(initialPath, { ...topology, rooms: [...topology.rooms, group] });
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.activeThreadStack.value).toEqual(["group-thread"]);

    await h.roomSync.selectChannel("general");
    await flush();
    h.discoverTopology.mockImplementation(async () => topology);
    await h.waddles.loadStructure("general");
    await flush();
    expect(h.historyEntries).toEqual([initialPath, "/r/general"]);
    expect(h.waddles.groupDms.value).toEqual([]);
    h.messaging.backfillThread.mockClear();

    h.history.back();
    const requestId = h.routeSync.beginRouteRequest();
    h.isApplyingRoute.value = true;
    await h.routeSync.applyRouteTarget(matchLocation(h.location.pathname, h.location.search), requestId, {
      intent: "explicit-navigation",
    });
    h.isApplyingRoute.value = false;
    await flush();

    await h.send.sendThreadMessage("must not go to General", [], [], undefined, undefined, { threadId: "group-thread" });
    expect(h.messaging.sendMessage).not.toHaveBeenCalled();
    expect(h.dmMessaging.sendMessage).not.toHaveBeenCalled();
    expect(h.waddles.activeChannelId.value).toBeNull();
    expect(h.conversation.activeTarget.value).toBeNull();
    expect(h.activeThreadStack.value).toEqual([]);
    expect(h.activeThreadTargetMessageId.value).toBeNull();
    expect(h.activeRightPanel.value).toBeNull();
    expect(h.ui.showPinnedPanel.value).toBe(false);
    expect(h.messaging.backfillThread).not.toHaveBeenCalled();
    expect(h.location.pathname + h.location.search).toBe("/dm");
  });

  test("explicit selection of a missing group leaves no old channel selected", async () => {
    const h = harness();
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.waddles.activeChannelId.value).toBe("general");

    expect(await h.roomSync.selectGroupDm("missing@muc.example.com")).toBe(false);
    await flush();
    expect(h.waddles.activeChannelId.value).toBeNull();
    expect(h.dmConversations.activePeerJid.value).toBeNull();
    expect(h.ui.sidebarMode.value).toBe("dms");
    expect(h.location.pathname).toBe("/dm");
  });

  test.each(["dm", "channel"])("selecting a %s writes one clean history entry and Back restores the previous thread", async (target) => {
    const initialPath = buildHref({
      id: "channel", params: { channelId: "general" }, search: { thread: ["general-thread"], pinned: true },
    });
    const random = { id: "random", name: "Random", jid: "random@muc.example.com", channelType: "text" as const };
    const h = harness(initialPath, { ...topology, rooms: [...topology.rooms, random] });
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.activeThreadStack.value).toEqual(["general-thread"]);

    if (target === "dm") await h.dmSync.selectDm("bob@example.com");
    else await h.roomSync.selectChannel(random.id);
    await flush();
    const entriesAfterSelection = [...h.historyEntries];

    h.history.back();
    // Apply the Back entry through the same route handler used by popstate.
    const requestId = h.routeSync.beginRouteRequest();
    h.isApplyingRoute.value = true;
    await h.routeSync.applyRouteTarget(matchLocation(h.location.pathname, h.location.search), requestId, {
      intent: "explicit-navigation",
    });
    h.isApplyingRoute.value = false;
    await flush();

    expect(entriesAfterSelection).toEqual([initialPath, target === "dm" ? "/dm/bob%40example.com" : "/r/random"]);
    expect(h.location.pathname + h.location.search).toBe(initialPath);
    expect(h.ui.sidebarMode.value).toBe("channels");
    expect(h.waddles.activeChannelId.value).toBe("general");
    expect(h.activeThreadStack.value).toEqual(["general-thread"]);
    expect(h.ui.showPinnedPanel.value).toBe(true);
  });

  for (const path of ["/dm/general%40example.com", "/dm/alice%40example.com"]) {
    test(`reselecting the visible DM supersedes initial ${path} discovery`, async () => {
      const h = harness(path);
      const finishDiscovery = delayDiscovery(h);
      h.connectionStore.appState = "ready";
      await flush();
      // A restored active peer can already be visible while the URL is loading.
      await h.dmConversations.openDm("bob@example.com");
      await flush();
      expect(h.location.pathname).toBe(path);

      let finishPresence!: () => void;
      const presence = new Promise<void>((resolve) => { finishPresence = resolve; });
      h.subscribeToPeerPresence.mockImplementation(() => presence);
      const selection = h.dmSync.selectDm("bob@example.com");
      finishDiscovery();
      await flush();
      const pathWhilePresenceLoads = h.location.pathname;
      finishPresence();
      await selection;

      expect(pathWhilePresenceLoads).toBe("/dm/bob%40example.com");
      expect(h.location.pathname).toBe("/dm/bob%40example.com");
      expect(h.ui.sidebarMode.value).toBe("dms");
      expect(h.dmConversations.activePeerJid.value).toBe("bob@example.com");
      expect(h.waddles.activeChannelId.value).toBeNull();
    });

    test.each(["selectDm", "handleOpenDm", "handleNewDm"] as const)(`%s supersedes initial ${path} discovery`, async (action) => {
      const h = harness(path);
      h.dmConversations.conversations.value = [{ peerJid: "bob@example.com", peerUsername: "bob", unreadCount: 0 }];
      const finishDiscovery = delayDiscovery(h);
      h.connectionStore.appState = "ready";
      await flush();
      expect(h.isApplyingRoute.value).toBe(true);

      if (action === "handleNewDm") await h.dmSync.searchDmRecipients("bob");
      await h.dmSync[action]("bob@example.com");
      finishDiscovery();
      await flush();

      expect(h.dmConversations.activePeerJid.value).toBe("bob@example.com");
      expect(h.ui.sidebarMode.value).toBe("dms");
      expect(h.waddles.activeChannelId.value).toBeNull();
      expect(h.location.pathname).toBe("/dm/bob%40example.com");
      expect(h.isApplyingRoute.value).toBe(false);
      expect(h.messaging.loadMessages).not.toHaveBeenCalled();
    });
  }

  test("a room chosen from the inbox supersedes a pending initial DM route", async () => {
    const h = harness("/dm/alice%40example.com");
    const finishDiscovery = delayDiscovery(h);
    h.connectionStore.appState = "ready";
    await flush();

    await h.roomSync.selectChannelByRoomJid("general@muc.example.com");
    finishDiscovery();
    await flush();

    expect(h.waddles.activeChannelId.value).toBe("general");
    expect(h.ui.sidebarMode.value).toBe("channels");
    expect(h.location.pathname).toBe("/r/general");
    expect(h.dmConversations.activePeerJid.value).toBeNull();
    expect(h.isApplyingRoute.value).toBe(false);
  });

  test.each([false, true])("an automatic conversation route keeps its own request (group: %s)", async (isGroupDm) => {
    const group = { id: "crew", name: "Crew", jid: "crew@muc.example.com", channelType: "text" as const, isGroupDm: true };
    const path = buildHref(isGroupDm
      ? { id: "groupDmRoom", params: { roomJid: group.jid }, search: { thread: ["saved-thread"], pinned: true } }
      : { id: "dm", params: { peerJid: "bob@example.com" }, search: { thread: ["saved-thread"], pinned: true } });
    const h = harness(path, { ...topology, rooms: [...topology.rooms, group] });
    h.connectionStore.appState = "ready";
    await flush();

    expect(h.isApplyingRoute.value).toBe(false);
    expect(h.ui.sidebarMode.value).toBe("dms");
    expect(h.activeThreadStack.value).toEqual(["saved-thread"]);
    expect(h.ui.showPinnedPanel.value).toBe(true);
    expect(h.location.pathname + h.location.search).toBe(path);
    if (isGroupDm) expect(h.waddles.currentChannel.value?.id).toBe("crew");
    else expect(h.dmConversations.activePeerJid.value).toBe("bob@example.com");
  });

  test.each([false, true])("a room choice cancels bootstrap while extension discovery waits (group: %s)", async (isGroupDm) => {
    const group = { id: "crew", name: "Crew", jid: "crew@muc.example.com", channelType: "text" as const, isGroupDm: true };
    const h = harness("/dm/alice%40example.com", { ...topology, rooms: [...topology.rooms, group] });
    await h.waddles.loadStructure(null, { noChannelSelect: true });
    let finishExtensions!: () => void;
    const pendingExtensions = new Promise<void>((resolve) => { finishExtensions = resolve; });
    h.refreshExtensionRoutes.mockImplementation(() => pendingExtensions);
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.refreshExtensionRoutes).toHaveBeenCalledTimes(1);

    if (isGroupDm) await h.roomSync.selectGroupDm(group.jid);
    else await h.roomSync.selectChannel("general");
    finishExtensions();
    await flush();

    expect(h.waddles.activeChannelId.value).toBe(isGroupDm ? group.id : "general");
    expect(h.ui.sidebarMode.value).toBe(isGroupDm ? "dms" : "channels");
    expect(h.dmConversations.activePeerJid.value).toBeNull();
    expect(h.isApplyingRoute.value).toBe(false);
    expect(h.dmMessaging.loadMessages).not.toHaveBeenCalled();
  });

  test("history navigation to a group DM keeps its route and explicit room retry", async () => {
    const group = { id: "crew", name: "Crew", jid: "crew@muc.example.com", channelType: "text" as const, isGroupDm: true };
    const h = harness("/dm", { ...topology, rooms: [...topology.rooms, group] });
    await h.waddles.loadStructure(null, { noChannelSelect: true });
    const requestId = h.routeSync.beginRouteRequest();
    h.isApplyingRoute.value = true;

    await h.routeSync.applyRouteTarget({
      id: "groupDmRoom", params: { roomJid: group.jid }, search: { thread: ["saved-thread"], pinned: true },
    }, requestId, { intent: "explicit-navigation" });

    expect(h.routeSync.isCurrentRouteRequest(requestId)).toBe(true);
    expect(h.activeThreadStack.value).toEqual(["saved-thread"]);
    expect(h.ui.showPinnedPanel.value).toBe(true);
    expect(h.messaging.loadMessages).toHaveBeenCalledWith("", group.id, 0, [], { intent: "explicit-navigation" });
  });

  test("Chat stays selected after reconnect discovery with rooms and no spaces", async () => {
    const h = harness();
    h.xmppStatus.value = { state: "online" };
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.waddles.activeChannelId.value).toBe("general");
    const beforeReconnect = h.discoverTopology.mock.calls.length;
    const finishDiscovery = delayDiscovery(h);
    h.xmppStatus.value = { state: "offline" };
    await flush();
    h.xmppStatus.value = { state: "online" };
    await flush();
    expect(h.discoverTopology).toHaveBeenCalledTimes(beforeReconnect + 1);

    h.page.openDmList();
    finishDiscovery();
    await flush();

    expect(h.location.pathname).toBe("/dm");
    expect(h.ui.sidebarMode.value).toBe("dms");
    expect(h.waddles.currentChannel.value).toBeNull();
    expect(h.waddles.channels.value).toHaveLength(1);
  });

  test("Chat cancels an initial channel route before discovery completes", async () => {
    const h = harness();
    const finishDiscovery = delayDiscovery(h);
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.isApplyingRoute.value).toBe(true);

    h.page.openDmList();
    finishDiscovery();
    await flush();

    expect(h.location.pathname).toBe("/dm");
    expect(h.ui.sidebarMode.value).toBe("dms");
    expect(h.waddles.activeChannelId.value).toBeNull();
    expect(h.isApplyingRoute.value).toBe(false);
    expect(h.messaging.loadMessages).not.toHaveBeenCalled();
  });

  test("a direct Chat route stays unselected after discovery", async () => {
    const h = harness("/dm");
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.location.pathname).toBe("/dm");
    expect(h.waddles.activeChannelId.value).toBeNull();
    expect(h.isApplyingRoute.value).toBe(false);
  });
});


describe("New DM recipient identity through the full shell", () => {
  test.each(["chat", "chat@example.com"])("selected account from %s stays a DM after delayed room discovery", async (input) => {
    const chat = { id: "chat", name: "Chat", jid: "chat@muc.example.com", channelType: "text" as const };
    const h = harness("/dm", { spaces: [], rooms: [chat] });
    h.searchUsers.mockResolvedValue([
      { id: "chat@example.com", jid: "chat@example.com", username: "chat", display_name: null, avatar_url: null },
    ]);
    const finishDiscovery = delayDiscovery(h);
    h.connectionStore.appState = "ready";
    await flush();
    const results = await h.dmSync.searchDmRecipients(input);
    await h.dmSync.handleNewDm(results[0]!.jid);
    await flush();
    expect(h.dmConversations.activePeerJid.value).toBe("chat@example.com");
    expect(h.location.pathname).toBe("/dm/chat%40example.com");
    await h.send.sendActiveMessage("local test message");
    expect(h.dmMessaging.sendMessage).toHaveBeenCalledTimes(1);
    expect(h.messaging.sendMessage).not.toHaveBeenCalled();
    finishDiscovery();
    await flush();
    expect(h.location.pathname).toBe("/dm/chat%40example.com");
    expect(h.dmConversations.activePeerJid.value).toBe("chat@example.com");
    expect(h.waddles.activeChannelId.value).toBeNull();
    expect(h.ui.sidebarMode.value).toBe("dms");
    expect(h.conversation.activeTarget.value).toBe(h.dmMessaging);
  });

  test("missing directory account never opens or becomes a room when discovery completes", async () => {
    const chat = { id: "chat", name: "Chat", jid: "chat@muc.example.com", channelType: "text" as const };
    const h = harness("/dm", { spaces: [], rooms: [chat] });
    h.searchUsers.mockResolvedValue([]);
    const finishDiscovery = delayDiscovery(h);
    h.connectionStore.appState = "ready";
    await flush();
    expect(await h.dmSync.searchDmRecipients("chat@example.com")).toEqual([]);
    await h.dmSync.handleNewDm("chat@example.com");
    finishDiscovery();
    await flush();
    expect(h.location.pathname).toBe("/dm");
    expect(h.dmConversations.conversations.value).toEqual([]);
    expect(h.dmConversations.activePeerJid.value).toBeNull();
    expect(h.waddles.activeChannelId.value).toBeNull();
    await h.send.sendActiveMessage("must not send");
    expect(h.dmMessaging.sendMessage).not.toHaveBeenCalled();
    expect(h.messaging.sendMessage).not.toHaveBeenCalled();
  });

  test.each([false, true])("a bare room DM route cannot retain the previous send target (group: %s)", async (group) => {
    const crew = { id: "crew", name: "Crew", jid: "crew@muc.example.com", channelType: "text" as const, isGroupDm: true };
    const h = harness("/dm", { ...topology, rooms: [...topology.rooms, crew] });
    h.connectionStore.appState = "ready";
    await flush();
    if (group) await h.roomSync.selectGroupDm(crew.jid);
    else await h.dmSync.selectDm("bob@example.com");
    await flush();
    await h.routeSync.applyRouteTarget({
      id: "dm", params: { peerJid: "general@muc.example.com" }, search: { thread: [], pinned: false },
    }, h.routeSync.beginRouteRequest());
    await h.send.sendActiveMessage("must not go to the old target");
    expect(h.dmConversations.activePeerJid.value).toBeNull();
    expect(h.waddles.activeChannelId.value).toBeNull();
    expect(h.conversation.activeTarget.value).toBeNull();
    expect(h.dmMessaging.sendMessage).not.toHaveBeenCalled();
    expect(h.messaging.sendMessage).not.toHaveBeenCalled();
  });

  test.each(["chat@elsewhere.example", "chat@muc.elsewhere.example/Nick/Device"])("existing peer %s survives URL round trip", async (peerJid) => {
    const h = harness("/dm");
    h.connectionStore.appState = "ready";
    await flush();
    h.dmConversations.conversations.value = [{
      peerJid, peerUsername: "chat", unreadCount: 0, ...(peerJid.includes("/") ? { mucPm: true } : {}),
    }];
    await h.dmSync.selectDm(peerJid);
    await flush();
    const match = matchLocation(h.location.pathname, h.location.search);
    expect(match).toEqual({ id: "dm", params: { peerJid }, search: { thread: [], pinned: false, ...(peerJid.includes("/") ? { scope: "occupant" } : {}) } });
    await h.routeSync.applyRouteTarget(match, h.routeSync.beginRouteRequest());
    expect(h.dmConversations.activePeerJid.value).toBe(peerJid);
    expect(h.dmMessaging.loadMessages).toHaveBeenLastCalledWith(peerJid, 0);
  });
});

describe("cold occupant routes", () => {
  test.each([false, true])("full occupant identity survives failed or partial discovery (partial=%s)", async (partial) => {
    const peerJid = "room@nondefault-muc.service/Nick/Device";
    const path = `/dm/${encodeURIComponent(peerJid)}`;
    const h = harness(`${path}?scope=occupant`);
    if (partial) h.discoverTopology.mockResolvedValue({ spaces: [], rooms: [] });
    else h.discoverTopology.mockRejectedValue(new Error("discovery unavailable"));
    h.connectionStore.appState = "ready";
    await flush();
    expect(h.dmConversations.activePeerJid.value).toBe(peerJid);
    expect(h.dmConversations.activeConversationScope.value).toBe("muc-occupant");
    expect(h.dmMessaging.loadMessages).toHaveBeenCalledWith(peerJid, 0);
    expect(h.subscribeToPeerPresence).not.toHaveBeenCalled();
    expect(h.location.pathname).toBe(path);
    expect(h.location.search).toBe("?scope=occupant");
    h.discoverTopology.mockResolvedValue({ spaces: [], rooms: [{ id: "room", name: "Room", jid: "room@nondefault-muc.service", channelType: "text" }] });
    await h.waddles.loadStructure(null, { noChannelSelect: true });
    await flush();
    expect(h.dmConversations.activePeerJid.value).toBe(peerJid);
    expect(h.dmConversations.conversations.value.map((conversation) => conversation.peerJid)).toEqual([peerJid]);
    await h.send.sendActiveMessage("private reply");
    expect(h.dmMessaging.sendMessage).toHaveBeenCalled();
    expect(h.messaging.sendMessage).not.toHaveBeenCalled();
  });

  test("an account resource is never inferred to be an occupant", async () => {
    const h = harness("/dm");
    await h.dmSync.handleOpenDm("bob@external.example/mobile");
    expect(h.dmConversations.activePeerJid.value).toBe("bob@external.example");
    expect(h.dmConversations.activeConversationScope.value).toBe("account");
    expect(matchLocation(`/dm/${encodeURIComponent("bob@external.example/mobile")}`)).toEqual({ id: "home" });
  });
});
