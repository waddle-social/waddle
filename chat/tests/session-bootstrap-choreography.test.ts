// Regression tests for #754: every session-ready used to re-fire the whole
// bootstrap fan-out (inbox hydrates, social feed / stories / events
// refreshes, notification-settings hydrate) from multiple subscribers —
// the session-lifecycle handler on BOTH fresh and resumed events, plus a
// duplicate tail in onConnectionReady. The contract under test: each
// bootstrap action fires exactly once per fresh session, and never on a
// XEP-0198 resume (a resume is gap-free — SM replays the push stream).

import { afterAll, beforeAll, describe, expect, mock, test } from "bun:test";
import { computed, effectScope, nextTick, ref, shallowReactive } from "vue";
import { useChatShellState } from "../src/shell/state";
import { useConnectionLifecycle } from "../src/shell/controllers/use-connection-lifecycle";
import { handlerStubs } from "./helpers/xmpp-client-mock";
import type { BrowserXmppClient, SessionLifecycleEvent } from "../src/lib/xmpp-client";
import type { WaddleSession } from "../src/lib/server-auth";

const session: WaddleSession = {
  session_id: "tok",
  user_id: "alice-id",
  username: "alice",
  avatar_url: null,
  xmpp_localpart: "alice",
  jid: "alice@example.com/desktop",
  xmpp_websocket_url: "wss://example.com/ws",
  is_expired: false,
  expires_at: null,
};

const freshEvent = {
  type: "fresh",
  catchup: { roomJids: [], dmJids: [] },
} as unknown as SessionLifecycleEvent;

function flushAsync(times = 4): Promise<void> {
  let p = Promise.resolve();
  for (let i = 0; i < times; i += 1) p = p.then(() => new Promise((r) => setTimeout(r, 0)));
  return p;
}

// onConnectionReady reads window.location through matchLocation; shim it
// for this file only and restore afterwards so other test files see the
// environment they expect.
const hadWindow = "window" in globalThis;
const originalWindow = (globalThis as Record<string, unknown>).window;

beforeAll(() => {
  (globalThis as Record<string, unknown>).window = {
    location: { pathname: "/", search: "", hash: "" },
  };
});

afterAll(() => {
  if (hadWindow) (globalThis as Record<string, unknown>).window = originalWindow;
  else delete (globalThis as Record<string, unknown>).window;
});

function makeHarness(options: {
  inboxHydrateRetryDelaysMs?: number[];
  dmInbox?: () => Promise<boolean>;
  channelInbox?: () => Promise<boolean>;
} = {}) {
  let lifecycleHandler: ((event: SessionLifecycleEvent) => void) | null = null;
  let mdsDisplayedHandler: ((entry: { chatId: string; stanzaId: string; stanzaIdBy: string }) => void) | null = null;
  const client = {
    ...handlerStubs(),
    setDirectMessageHandler: () => {},
    setDmChatStateHandler: () => {},
    setDmDisplayedHandler: () => {},
    setDmReactionHandler: () => {},
    setMdsDisplayedHandler: (handler: typeof mdsDisplayedHandler) => { mdsDisplayedHandler = handler; },
    setPresenceUpdateHandler: () => {},
    addPubsubEventHandler: () => {},
    setMemberJidHandler: () => {},
    setMessageAckHandler: () => {},
    setMessageDeliveryFailureHandler: () => {},
    setQueuedMessageStatusHandler: () => {},
    setInboxPushHandler: () => {},
    setCatchupFailureHandler: () => {},
    setSessionLifecycleHandler: (h: (event: SessionLifecycleEvent) => void) => {
      lifecycleHandler = h;
    },
  } as unknown as BrowserXmppClient;

  const connectionStore = shallowReactive({
    client,
    appState: "loading",
    session,
    appError: "",
    activeServerUrl: "",
    providers: [],
    login: async () => {},
    logout: async () => {},
    fetchProviders: async () => {},
    bootstrap: async () => {},
  });

  const hydrateDmInbox = mock(options.dmInbox ?? (async () => true));
  const hydrateChannelInbox = mock(options.channelInbox ?? (async () => true));
  const refreshSocialFeed = mock(async () => {});
  const refreshStories = mock(async () => {});
  const refreshCommunityEvents = mock(async () => {});
  const hydrateNotifySettings = mock(async () => {});
  const loadRosterContacts = mock(async () => {});
  const presenceOnSessionReady = mock(() => {});

  const ui = useChatShellState();
  const deps = {
    ui,
    connectionStore: connectionStore as never,
    xmppClient: computed(() => client),
    session: computed(() => session),
    waddles: {
      waddles: ref([]),
      channels: ref([]),
      groupDms: ref([]),
      activeChannelId: ref<string | null>(null),
      isLoadingStructure: ref(false),
      hasLoadedStructure: ref(false),
      loadStructure: mock(async () => {}),
      clearData: mock(() => {}),
    },
    messaging: {
      onSessionLifecycle: mock(() => {}),
      onCatchupFailed: mock(() => {}),
      onMessageAck: mock(() => {}),
      onMessageDeliveryFailure: mock(() => {}),
      onMessageQueueStatus: mock(() => {}),
      applyMdsDisplayed: mock(() => true),
      xmppStatus: ref({ state: "offline" }),
      disconnect: mock(() => {}),
      clearMessages: mock(() => {}),
    },
    dmMessaging: {
      onIncomingMessage: mock(() => {}),
      onChatState: mock(() => {}),
      onDisplayed: mock(() => {}),
      onReaction: mock(() => {}),
      onSessionLifecycle: mock(() => {}),
      onCatchupFailed: mock(() => {}),
      onMessageAck: mock(() => {}),
      onMessageDeliveryFailure: mock(() => {}),
      onMessageQueueStatus: mock(() => {}),
      applyMdsDisplayed: mock(() => true),
      disconnect: mock(() => {}),
      clearMessages: mock(() => {}),
    },
    dmConversations: {
      hydrateFromInbox: hydrateDmInbox,
      receiveIncomingDm: mock(() => {}),
      onInboxPush: mock(() => {}),
    },
    channelUnread: {
      hydrateFromInbox: hydrateChannelInbox,
      onInboxPush: mock(() => {}),
      clearAll: mock(() => {}),
    },
    rosterContacts: {
      loadRosterContacts,
      clearRosterContacts: mock(() => {}),
    },
    socialFeed: { refresh: refreshSocialFeed },
    stories: { refresh: refreshStories },
    communityEvents: { refresh: refreshCommunityEvents },
    notifications: { registerServiceWorker: mock(async () => {}) },
    notifySettings: {
      hydrate: hydrateNotifySettings,
      reset: mock(() => {}),
    },
    appUpdate: { start: mock(async () => {}), stop: mock(() => {}) },
    isApplyingRoute: ref(false),
    memberJidByNick: ref({}),
    extensionRoutes: ref([]),
    activeExtensionRouteKey: ref(null),
    activeRightPanel: ref(null),
    selectedChannelRoomJids: ref({}),
    isActiveDirectDmSurface: () => false,
    inboxHydrateRetryDelaysMs: options.inboxHydrateRetryDelaysMs ?? [],
    presence: {
      onClientCleared: mock(() => {}),
      handlePresenceUpdate: mock(() => {}),
      handleActivityPubsubEvent: mock(() => {}),
      handleStatusPreferencePubsubEvent: mock(() => {}),
      onSessionReady: presenceOnSessionReady,
      flushAndResetForLogout: mock(async () => {}),
    },
    notificationOrchestration: {
      notifyDmActivity: mock(() => {}),
      setupPushSubscription: mock(async () => {}),
    },
    routeSync: {
      beginRouteRequest: mock(() => 1),
      isCurrentRouteRequest: mock(() => true),
      applyRouteTarget: mock(async () => {}),
      updateUrl: mock(() => {}),
    },
    refreshExtensionRoutes: mock(async () => {}),
    clearPendingChannelRoomJidSelection: mock(() => {}),
    showFirstRunSetupIfNeeded: mock(() => {}),
    resetSetupPrompt: mock(() => {}),
  };

  const scope = effectScope();
  scope.run(() => {
    useConnectionLifecycle(deps as never);
  });

  return {
    scope,
    connectionStore,
    dispatchLifecycle: (event: SessionLifecycleEvent) => {
      if (!lifecycleHandler) throw new Error("session lifecycle handler was not registered");
      lifecycleHandler(event);
    },
    dispatchMdsDisplayed: (entry: { chatId: string; stanzaId: string; stanzaIdBy: string }) => {
      if (!mdsDisplayedHandler) throw new Error("MDS displayed handler was not registered");
      mdsDisplayedHandler(entry);
    },
    dmApplyMdsDisplayed: deps.dmMessaging.applyMdsDisplayed,
    roomApplyMdsDisplayed: deps.messaging.applyMdsDisplayed,
    counts: () => ({
      dmInbox: hydrateDmInbox.mock.calls.length,
      channelInbox: hydrateChannelInbox.mock.calls.length,
      socialFeed: refreshSocialFeed.mock.calls.length,
      stories: refreshStories.mock.calls.length,
      communityEvents: refreshCommunityEvents.mock.calls.length,
      notifySettings: hydrateNotifySettings.mock.calls.length,
    }),
    presenceOnSessionReady,
  };
}

describe("session bootstrap choreography (#754)", () => {
  test("cross-resource MDS keeps an unknown second occupant in one room isolated", async () => {
    const harness = makeHarness();
    harness.dispatchMdsDisplayed({
      chatId: "room@conference.example/alice",
      stanzaId: "sid-alice",
      stanzaIdBy: "example.com",
    });
    harness.dispatchMdsDisplayed({
      chatId: "room@conference.example/bob",
      stanzaId: "sid-bob",
      stanzaIdBy: "example.com",
    });

    expect(harness.roomApplyMdsDisplayed).toHaveBeenNthCalledWith(1,
      "room@conference.example/alice",
      { stanzaId: "sid-alice", stanzaIdBy: "example.com" },
    );
    expect(harness.roomApplyMdsDisplayed).toHaveBeenNthCalledWith(2,
      "room@conference.example/bob",
      { stanzaId: "sid-bob", stanzaIdBy: "example.com" },
    );
    harness.scope.stop();
  });
  test("a resumed session-ready re-hydrates each surface exactly once (inbox pushes are not SM-replayed)", async () => {
    // XEP-0198 resume replays peer-routed stanzas, but the server
    // fire-and-forgets inbox pushes to live resources only — a detached
    // session misses them (e.g. cross-device mark-read while detached),
    // so a resume must still re-hydrate. The #754 fix is ONE fire per
    // session-ready, not zero on resume. XEP-0492 notification settings
    // stay fresh-only: bookmark publishes ride the PEP queue, which a
    // resume genuinely preserves.
    const h = makeHarness();
    await nextTick();

    h.dispatchLifecycle({ type: "resumed" } as SessionLifecycleEvent);
    await flushAsync();

    expect(h.counts()).toEqual({
      dmInbox: 1,
      channelInbox: 1,
      socialFeed: 1,
      stories: 1,
      communityEvents: 1,
      notifySettings: 0,
    });
    // Presence re-assertion is per-session-ready and must keep firing.
    expect(h.presenceOnSessionReady.mock.calls.length).toBe(1);
    h.scope.stop();
  });

  test("a fresh session-ready fires each bootstrap action exactly once", async () => {
    const h = makeHarness();
    await nextTick();

    h.dispatchLifecycle(freshEvent);
    await flushAsync();

    expect(h.counts()).toEqual({
      dmInbox: 1,
      channelInbox: 1,
      socialFeed: 1,
      stories: 1,
      communityEvents: 1,
      notifySettings: 1,
    });
    h.scope.stop();
  });

  test("first connect (app-ready then fresh session event) fires each bootstrap action exactly once", async () => {
    const h = makeHarness();
    await nextTick();

    // App shell becomes ready first (HTTP session load), then the XMPP
    // connection comes up and emits the fresh lifecycle event.
    h.connectionStore.appState = "ready";
    await nextTick();
    await flushAsync();
    h.dispatchLifecycle(freshEvent);
    await flushAsync();

    expect(h.counts()).toEqual({
      dmInbox: 1,
      channelInbox: 1,
      socialFeed: 1,
      stories: 1,
      communityEvents: 1,
      notifySettings: 1,
    });
    h.scope.stop();
  });

  test("two fresh reconnects re-hydrate once each (per-session, not per-subscriber)", async () => {
    const h = makeHarness();
    await nextTick();

    h.dispatchLifecycle(freshEvent);
    await flushAsync();
    h.dispatchLifecycle(freshEvent);
    await flushAsync();

    expect(h.counts()).toEqual({
      dmInbox: 2,
      channelInbox: 2,
      socialFeed: 2,
      stories: 2,
      communityEvents: 2,
      notifySettings: 2,
    });
    h.scope.stop();
  });

  test("a failed inbox hydrate retries with backoff until it succeeds", async () => {
    // With MAM catch-up re-emissions now side-effect-free, the inbox
    // hydrate is the sole unread source for messages missed while
    // disconnected — a single failed IQ must not strand badges at zero
    // until the next session-ready.
    let dmAttempts = 0;
    const h = makeHarness({
      inboxHydrateRetryDelaysMs: [0, 0],
      dmInbox: async () => {
        dmAttempts += 1;
        return dmAttempts >= 2;
      },
    });
    await nextTick();

    h.dispatchLifecycle(freshEvent);
    await flushAsync();

    // Succeeds on the second attempt; the remaining delay is not consumed.
    expect(h.counts().dmInbox).toBe(2);
    // The channel hydrate succeeded immediately — no retries.
    expect(h.counts().channelInbox).toBe(1);
    h.scope.stop();
  });

  test("a persistently failing inbox hydrate gives up after the configured retries", async () => {
    const h = makeHarness({
      inboxHydrateRetryDelaysMs: [0],
      dmInbox: async () => false,
    });
    await nextTick();

    h.dispatchLifecycle(freshEvent);
    await flushAsync();

    // Initial attempt + exactly one retry.
    expect(h.counts().dmInbox).toBe(2);
    h.scope.stop();
  });
});
