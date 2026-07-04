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
    // bare JID of either the MUC room or the DM peer.
    client.setMdsDisplayedHandler((entry) => {
      const chatId = barePeerJid(entry.chatId);
      const displayed = {
        stanzaId: entry.stanzaId,
        stanzaIdBy: barePeerJid(entry.stanzaIdBy),
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
      // Re-hydrate inbox on every XMPP session-ready, both resumed and
      // fresh. Stream resume catches up on stanzas the client missed
      // while disconnected, but a *fresh* reconnection (resume failed
      // — too much time elapsed, server restart, network blip past the
      // resume window) means we lost the push stream entirely and the
      // local unread map is stale. `onConnectionReady` only hydrates
      // on the first sign-in (one-shot `hasBootstrapped` guard), so
      // subsequent fresh reconnections would otherwise never refresh.
      // `hydrateFromInbox` is request-id deduped, so the redundant
      // call on the very first connection is harmless.
      void dmConversations.hydrateFromInbox();
      void channelUnread.hydrateFromInbox();
      void socialFeed.refresh();
      void stories.refresh();
      void communityEvents.refresh();
      presence.onSessionReady(event, client);
      // Re-hydrate XEP-0492 notification settings only on *fresh*
      // reconnects. A stream resume is by definition gap-free —
      // any bookmark publish from another tab during the disconnect
      // is impossible because we never disconnected as far as the
      // server's PEP queue is concerned. Refetching on every resume
      // burns one IQ round-trip per resume for no payoff (round-12
      // reviewer P2). Until the chat subscribes to PEP `+notify`
      // headlines on `urn:xmpp:bookmarks:1` (deferred follow-up),
      // fresh-only hydrate is the correct cadence.
      if (event.type === "fresh") {
        // Belt-and-braces: hydrate already catches lower-layer
        // throws, but call-site .catch defends against any future
        // regression so an unhandled rejection doesn't propagate
        // out of the lifecycle handler. Round-14 PR review.
        notifySettings.hydrate(client).catch(() => undefined);
      }
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
    return match.id === "channel" || match.id === "channelExtension" || match.id === "groupDmRoom";
  }

  function channelRouteTargetMissing(match: RouteMatch): boolean {
    if (match.id === "groupDmRoom") {
      const roomJid = barePeerJid(match.params.roomJid);
      return !waddles.groupDms.value.some((group) => barePeerJid(group.roomJid) === roomJid);
    }
    if (match.id !== "channel" && match.id !== "channelExtension") return false;
    return resolveChannelBySlug(match.params.channelId, waddles.channels.value) == null;
  }

  async function applyPendingChannelRouteAfterStructure() {
    if (!pendingChannelRouteMatch || waddles.channels.value.length === 0) return;
    const match = matchLocation(window.location.pathname, window.location.search);
    if (!routeNeedsDiscoveredChannel(match)) {
      pendingChannelRouteMatch = null;
      return;
    }
    if (channelRouteTargetMissing(match)) return;
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

    void dmConversations.hydrateFromInbox();
    void channelUnread.hydrateFromInbox();
    void rosterContacts.loadRosterContacts();
    void socialFeed.refresh();

    // Hydrate XEP-0492 per-chat notification settings from the user's
    // XEP-0402 PEP bookmarks. Best-effort — an empty result is the
    // first-run state and the UI falls back to the §3 conversation
    // default via [[effectiveNotifyMode]].
    void (async () => {
      const client = xmppClient.value;
      if (!client) return;
      // Best-effort: hydrate already swallows lower-layer
      // exceptions, but a defensive .catch keeps the IIFE quiet
      // even if a future regression bypasses the inner guard.
      // Round-14 PR review.
      await notifySettings.hydrate(client).catch(() => undefined);
    })();

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
