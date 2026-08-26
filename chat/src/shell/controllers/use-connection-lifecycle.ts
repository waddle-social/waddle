import { type ComputedRef, onUnmounted, type Ref, watch } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useChannelInbox } from "@/channels/inbox";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { useXmppRosterContacts } from "@/contacts/roster";
import type { useSocialFeed } from "@/services/social-feed";
import type { useStories } from "@/services/stories";
import type { useCommunityEvents } from "@/services/community-events";
import type { useServiceWorkerUpdate } from "@/shell/service-worker-update";
import type { usePushNotifications } from "@/shell/notifications";
import type { ChatShellState } from "@/shell/state";
import {
  shouldPreserveActiveChannelDuringStructureRetry,
  shouldRetryMissingStructureLoad,
} from "@/shell/structure-retry";
import type { connectionStore as ConnectionStore } from "@/lib/connection-store";
import type { NotifySettingsStore } from "@/lib/notify-settings";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { WaddleSession } from "@/lib/server-auth";
import { barePeerJid } from "@/lib/xmpp-client";
import { bareJidKey, fullJidIdentityKey, resourceOf } from "@/lib/xmpp/jid";
import { mdsChatKey, queueMdsDisplayed, setMdsDisplayed } from "@/lib/last-seen-store";
import { resetPinnedRooms } from "@/stores/pinned-messages";
import { matchLocation, navigate, type RouteMatch } from "@/router";
import { resolveChannelBySlug } from "@/shell/route-helpers";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import type { usePresenceSync } from "@/shell/controllers/use-presence-sync";
import type { useNotificationOrchestration } from "@/shell/controllers/use-notification-orchestration";
import type { useRouteSync } from "@/shell/controllers/use-route-sync";
import type { ActiveRightPanel } from "@/shell/controllers/use-thread-panels";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";

interface ConnectionLifecycleDeps {
  ui: ChatShellState;
  connectionStore: typeof ConnectionStore;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  channelUnread: ReturnType<typeof useChannelInbox>;
  rosterContacts: ReturnType<typeof useXmppRosterContacts>;
  socialFeed: ReturnType<typeof useSocialFeed>;
  stories: ReturnType<typeof useStories>;
  communityEvents: ReturnType<typeof useCommunityEvents>;
  notifications: ReturnType<typeof usePushNotifications>;
  notifySettings: NotifySettingsStore;
  appUpdate: ReturnType<typeof useServiceWorkerUpdate>;
  isApplyingRoute: Ref<boolean>;
  memberJidByNick: Ref<Record<string, string>>;
  extensionRoutes: Ref<DiscoveredExtensionRoute[]>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  selectedChannelRoomJids: Ref<Record<string, string>>;
  isActiveDirectDmSurface: () => boolean;
  /** Backoff between inbox-hydrate retries; injectable for tests. */
  inboxHydrateRetryDelaysMs?: number[];
  presence: ReturnType<typeof usePresenceSync>;
  notificationOrchestration: ReturnType<typeof useNotificationOrchestration>;
  routeSync: ReturnType<typeof useRouteSync>;
  refreshExtensionRoutes: () => Promise<void>;
  clearPendingChannelRoomJidSelection: () => void;
  showFirstRunSetupIfNeeded: () => void;
  resetSetupPrompt: () => void;
}

/**
 * Connection lifecycle: registers every inbound handler on the XMPP
 * client, re-hydrates state on session-ready, bootstraps the app (initial
 * structure load + route application) when the connection becomes ready,
 * retries missing structure after reconnects, and tears everything down
 * on logout.
 */
export function useConnectionLifecycle(deps: ConnectionLifecycleDeps) {
  const {
    ui,
    connectionStore,
    xmppClient,
    session,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    channelUnread,
    rosterContacts,
    socialFeed,
    stories,
    communityEvents,
    notifications,
    notifySettings,
    appUpdate,
    isApplyingRoute,
    memberJidByNick,
    extensionRoutes,
    activeExtensionRouteKey,
    activeRightPanel,
    selectedChannelRoomJids,
    isActiveDirectDmSurface,
    presence,
    notificationOrchestration,
    routeSync,
    refreshExtensionRoutes,
    clearPendingChannelRoomJidSelection,
    showFirstRunSetupIfNeeded,
    resetSetupPrompt,
  } = deps;

  // #754: the single bootstrap choreographer. Every per-session hydrate
  // lives here and fires exactly ONCE per session-ready — from the
  // session-lifecycle handler only. onConnectionReady no longer
  // duplicates this set (appState turns "ready" on the HTTP session
  // load, before XMPP even connects — the lifecycle event of that first
  // connection covers it).
  //
  // Resumed sessions re-hydrate too: XEP-0198 resume replays peer-routed
  // stanzas, but the server fire-and-forgets inbox pushes to LIVE
  // resources only — a detached session misses them (e.g. cross-device
  // mark-read while detached), so the local unread map can be stale
  // after any resume.
  // With catch-up re-emissions no longer bumping unread locally, the
  // inbox hydrate is the sole unread source for messages missed while
  // disconnected — a single failed IQ on a still-flaky reconnect link
  // must not leave badges stuck at zero until the next session.
  const inboxHydrateRetryDelaysMs = deps.inboxHydrateRetryDelaysMs ?? [2_000, 8_000];
  function hydrateInboxWithRetry(hydrate: () => Promise<boolean>) {
    // Bound the chain to the session that started it: after a
    // logout→re-login inside the backoff window the new session runs its
    // own bootstrap — a stale chain firing into it would supersede that
    // hydrate and burn its retry budget.
    const ownerJid = connectionStore.session?.jid ?? null;
    void (async () => {
      if (await hydrate()) return;
      for (const delayMs of inboxHydrateRetryDelaysMs) {
        await new Promise((resolve) => setTimeout(resolve, delayMs));
        if ((connectionStore.session?.jid ?? null) !== ownerJid || !connectionStore.session) return;
        if (await hydrate()) return;
      }
    })();
  }

  function runSessionBootstrap(client: BrowserXmppClient, lifecycle: "fresh" | "resumed") {
    hydrateInboxWithRetry(() => dmConversations.hydrateFromInbox());
    hydrateInboxWithRetry(() => channelUnread.hydrateFromInbox());
    void socialFeed.refresh();
    void stories.refresh();
    void communityEvents.refresh();
    // XEP-0492 notification settings hydrate on fresh only: bookmark
    // publishes ride the PEP queue, which a resume genuinely preserves.
    // Belt-and-braces .catch so an unhandled rejection can't propagate
    // out of the lifecycle handler.
    if (lifecycle === "fresh") {
      notifySettings.hydrate(client).catch(() => undefined);
    }
  }

  watch(xmppClient, (client) => {
    if (!client || !session.value) {
      presence.onClientCleared();
      return;
    }
    client.setDirectMessageHandler((msg) => {
      dmMessaging.onIncomingMessage(msg);
      dmConversations.receiveIncomingDm(msg);
      notificationOrchestration.notifyDmActivity(msg);
    });
    client.setDmChatStateHandler(dmMessaging.onChatState);
    client.setDmDisplayedHandler(dmMessaging.onDisplayed);
    client.setDmReactionHandler(dmMessaging.onReaction);
    // XEP-0490 §3.2: another resource of this account has marked a
    // chat as displayed. Persist the stanza-id under the MDS-scoped
    // last-seen key so existing conversation-scoped readers can pick
    // it up alongside their own divider state. The chat-id is the
    // bare JID for DMs/rooms and full occupant JID for MUC PMs.
    client.setMdsDisplayedHandler((entry) => {
      // The XEP-0490 item id is already the protocol's typed chat
      // identity. Do not reclassify it through mutable local discovery:
      // an event for a not-yet-opened occupant must remain full too.
      const chatId = resourceOf(entry.chatId)
        ? fullJidIdentityKey(entry.chatId)
        : bareJidKey(entry.chatId);
      const displayed = {
        stanzaId: entry.stanzaId,
        stanzaIdBy: entry.stanzaIdBy,
      };
      const accepted = isActiveDirectDmSurface()
        ? dmMessaging.applyMdsDisplayed(chatId, displayed)
        : messaging.applyMdsDisplayed(chatId, displayed);
      const key = mdsChatKey(chatId);
      if (accepted) setMdsDisplayed(key, displayed);
      else queueMdsDisplayed(key, displayed);
    });
    client.setPresenceUpdateHandler(presence.handlePresenceUpdate);
    client.addPubsubEventHandler(presence.handleActivityPubsubEvent);
    client.addPubsubEventHandler(presence.handleStatusPreferencePubsubEvent);
    client.setMemberJidHandler((nick, bareJid) => {
      memberJidByNick.value = { ...memberJidByNick.value, [nick]: bareJid };
    });
    // XEP-0198 fan-out: the same message ID only ever appears in one timeline
    // (rooms vs DMs), so calling both is idempotent - whichever owns the id
    // updates it, the other no-ops.
    client.setMessageAckHandler((id) => {
      messaging.onMessageAck(id);
      dmMessaging.onMessageAck(id);
    });
    client.setMessageDeliveryFailureHandler((id) => {
      messaging.onMessageDeliveryFailure(id);
      dmMessaging.onMessageDeliveryFailure(id);
    });
    client.setQueuedMessageStatusHandler((id, status) => {
      messaging.onMessageQueueStatus(id, status);
      dmMessaging.onMessageQueueStatus(id, status);
    });
    client.setInboxPushHandler((entry) => {
      dmConversations.onInboxPush(entry);
      channelUnread.onInboxPush(entry);
    });
    // #1180: the fresh lifecycle event's coverage made the composables
    // skip their MAM reload; a covered-but-failed catch-up hands the
    // reload back to them here, after the failed attempt.
    client.setCatchupFailureHandler((failure) => {
      messaging.onCatchupFailed(failure);
      dmMessaging.onCatchupFailed(failure);
    });
    client.setSessionLifecycleHandler((event) => {
      messaging.onSessionLifecycle(event);
      dmMessaging.onSessionLifecycle(event);
      // Short-circuit if the session is already torn down — a
      // lifecycle event queued before `handleLogout` ran can fire
      // here AFTER `notifySettings.reset()` and would
      // otherwise restart hydrate against the about-to-disconnect
      // client. Round-12 reviewer P1.
      if (!connectionStore.session) return;
      presence.onSessionReady(event, client);
      // #754: one bootstrap fire per session-ready, fresh or resumed —
      // the choreographer replaces the old multi-subscriber fan-out
      // that fired 4-6 copies of each IQ per reconnect.
      runSessionBootstrap(client, event.type);
    });
  }, { immediate: true });

  // --- Bootstrap (watches connection store) ---

  let initialStructureLoadFinished = false;
  let missingStructureOnlineEpoch = messaging.xmppStatus.value.state === "online" ? 1 : 0;
  let lastMissingStructureRefreshEpoch = 0;
  let missingStructureRefreshPromise: Promise<void> | null = null;
  let pendingChannelRouteMatch: RouteMatch | null = null;

  function clearPendingChannelRoute() {
    pendingChannelRouteMatch = null;
  }

  function routeNeedsDiscoveredChannel(match: RouteMatch): boolean {
    return match.id === "channel" || match.id === "channelExtension" || match.id === "groupDmRoom" || match.id === "dm";
  }

  function channelRouteTargetMissing(match: RouteMatch): boolean {
    if (match.id === "dm") {
      return waddles.hasLoadedStructure.value === false;
    }
    if (match.id === "groupDmRoom") {
      const roomJid = barePeerJid(match.params.roomJid);
      return !waddles.groupDms.value.some((group) => barePeerJid(group.roomJid) === roomJid);
    }
    if (match.id !== "channel" && match.id !== "channelExtension") return false;
    return resolveChannelBySlug(match.params.channelId, waddles.channels.value) == null;
  }

  async function applyPendingChannelRouteAfterStructure() {
    if (!pendingChannelRouteMatch) return;
    const match = matchLocation(window.location.pathname, window.location.search);
    if (match.id === "dm") {
      if (waddles.hasLoadedStructure.value === false) return;
    } else if (waddles.channels.value.length === 0) {
      return;
    }
    if (!routeNeedsDiscoveredChannel(match)) {
      pendingChannelRouteMatch = null;
      return;
    }
    if (match.id !== "dm" && channelRouteTargetMissing(match)) return;
    pendingChannelRouteMatch = null;
    const requestId = routeSync.beginRouteRequest();
    isApplyingRoute.value = true;
    try {
      await refreshExtensionRoutes();
      if (routeSync.isCurrentRouteRequest(requestId)) {
        await routeSync.applyRouteTarget(match, requestId);
      }
    } finally {
      if (routeSync.isCurrentRouteRequest(requestId)) {
        isApplyingRoute.value = false;
        routeSync.updateUrl();
      }
    }
  }

  async function refreshMissingStructureAfterReconnect() {
    const retryEpoch = missingStructureOnlineEpoch;
    const currentMatch = matchLocation(window.location.pathname, window.location.search);
    const routeTargetMissing = channelRouteTargetMissing(currentMatch);
    pendingChannelRouteMatch = routeTargetMissing ? currentMatch : null;
    if (!shouldRetryMissingStructureLoad({
      appReady: connectionStore.appState === "ready",
      hasClient: xmppClient.value !== null,
      initialLoadFinished: initialStructureLoadFinished,
      inFlight: missingStructureRefreshPromise !== null,
      isLoadingStructure: waddles.isLoadingStructure.value,
      spaceCount: waddles.waddles.value.length,
      channelCount: waddles.channels.value.length,
      routeTargetMissing,
      xmppStatus: messaging.xmppStatus.value.state,
      onlineEpoch: retryEpoch,
      lastAttemptedOnlineEpoch: lastMissingStructureRefreshEpoch,
    })) {
      return;
    }

    lastMissingStructureRefreshEpoch = retryEpoch;
    const activeChannelId = waddles.activeChannelId.value;
    const preserveActiveChannel = shouldPreserveActiveChannelDuringStructureRetry({
      activeChannelListed: activeChannelId !== null && waddles.channels.value.some((channel) => channel.id === activeChannelId),
      routeTargetMissing,
    });
    const promise = (
      preserveActiveChannel
        ? waddles.loadStructure(activeChannelId)
        : waddles.loadStructure(null, { noChannelSelect: true })
    ).then(() => undefined);
    missingStructureRefreshPromise = promise;
    try {
      await promise;
      await applyPendingChannelRouteAfterStructure();
    } finally {
      if (missingStructureRefreshPromise === promise) {
        missingStructureRefreshPromise = null;
      }
    }
  }

  async function onConnectionReady() {
    const match = matchLocation(window.location.pathname, window.location.search);
    const requestId = routeSync.beginRouteRequest();
    isApplyingRoute.value = true;
    let preserveCurrentUrl = false;

    try {
      // Always pass noChannelSelect — channel-targeting routes
      // (`channel` / `channelExtension`) get their active channel set
      // by applyRouteTarget from match.params.channelId, and every
      // other route doesn't want a channel active at all. Auto-select
      // would briefly highlight an arbitrary channel before
      // applyRouteTarget cleared it (visible flicker on /events, /feed,
      // /stories, /threads, …).
      try {
        if (waddles.channels.value.length === 0) {
          await waddles.loadStructure(null, { noChannelSelect: true });
        }
      } finally {
        initialStructureLoadFinished = true;
      }
      if (channelRouteTargetMissing(match)) {
        pendingChannelRouteMatch = match;
      }
      await refreshMissingStructureAfterReconnect();
      if (channelRouteTargetMissing(match)) {
        pendingChannelRouteMatch = match;
        preserveCurrentUrl = true;
      } else {
        await refreshExtensionRoutes();
      }
      if (!preserveCurrentUrl && routeSync.isCurrentRouteRequest(requestId)) {
        await routeSync.applyRouteTarget(match, requestId);
      }
      showFirstRunSetupIfNeeded();
    } finally {
      if (routeSync.isCurrentRouteRequest(requestId)) {
        isApplyingRoute.value = false;
        if (!preserveCurrentUrl) {
          routeSync.updateUrl();
        }
      }
    }

    // #754: inbox/feed/stories/events/notify-settings hydrates are NOT
    // fired here — the fresh session-lifecycle event of the first XMPP
    // connection runs them exactly once via runSessionBootstrap (the
    // client is registered in the store before it lazily connects, so
    // that event cannot be missed).
    void rosterContacts.loadRosterContacts();

    // Register service worker and sync push subscription (best-effort, non-blocking)
    void (async () => {
      await notifications.registerServiceWorker();
      await notificationOrchestration.setupPushSubscription();
    })();
  }

  async function handleLogout() {
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "dashboard";
    messaging.disconnect();
    dmMessaging.disconnect();
    waddles.clearData();
    channelUnread.clearAll();
    rosterContacts.clearRosterContacts();
    extensionRoutes.value = [];
    selectedChannelRoomJids.value = {};
    activeExtensionRouteKey.value = null;
    activeRightPanel.value = null;
    resetSetupPrompt();
    messaging.clearMessages();
    dmMessaging.clearMessages();
    // #414: drop all pin state on logout so a subsequent login doesn't
    // see the prior user's pinned-message previews and pre-hydration
    // events buffered from the prior session don't leak forward.
    resetPinnedRooms();
    // #532: drop the XEP-0492 settings cache so a subsequent
    // sign-in does not leak the previous account's per-chat modes
    // into UI reads while the fresh `hydrate` is still in flight.
    notifySettings.reset();
    await presence.flushAndResetForLogout();
    ui.showPinnedPanel.value = false;
    navigate({ id: "home" });
    await connectionStore.logout();
  }

  // Watch for connection becoming ready (XmppProvider handles auth bootstrap)
  let hasBootstrapped = false;
  watch(
    () => connectionStore.appState,
    (state) => {
      if (state === "ready") {
        void appUpdate.start();
        return;
      }

      appUpdate.stop();
    },
    { immediate: true },
  );

  watch(
    () => connectionStore.appState,
    (state) => {
      if (state === "ready" && !hasBootstrapped) {
        hasBootstrapped = true;
        void onConnectionReady();
      }
    },
    { immediate: true },
  );

  watch(
    () => messaging.xmppStatus.value.state,
    (state, previousState) => {
      if (state === "online" && previousState !== "online") {
        missingStructureOnlineEpoch += 1;
        void refreshMissingStructureAfterReconnect();
      }
    },
  );

  onUnmounted(() => {
    appUpdate.stop();
    messaging.disconnect();
    dmMessaging.disconnect();
  });

  return {
    clearPendingChannelRoute,
    handleLogout,
  };
}
