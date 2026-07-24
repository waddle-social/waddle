import { type ComputedRef, onMounted, onUnmounted, type Ref } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { WaddleSession } from "@/lib/server-auth";
import { jidDomain } from "@/lib/xmpp-client";
import { matchLocation, navigate, type RouteMatch } from "@/router";
import { resolveChannelBySlug } from "@/shell/route-helpers";
import type { ActiveRightPanel } from "@/shell/controllers/use-thread-panels";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";
import type { ChannelLoadIntent } from "@/channels/room-access";

/**
 * Single mapping from the typed route match to the page-level shell
 * state (activePage, activeCommunitySurface, sidebarMode,
 * showPinnedPanel). Used by the SSR seed at controller construction
 * and at the top of `applyRouteTarget` — keeping the derivation in
 * one place avoids the seed and the popstate path drifting apart.
 */
export function applyMatchToShellState(ui: ChatShellState, match: RouteMatch): void {
  switch (match.id) {
    case "home":
      ui.activePage.value = "dashboard";
      break;
    case "channel":
    case "channelExtension":
    case "dm":
    case "groupDmRoom":
    case "dmList":
    case "feed":
    case "stories":
    case "events":
      ui.activePage.value = "chat";
      break;
    case "settings":
      ui.activePage.value = "settings";
      break;
    case "admin":
      ui.activePage.value = "admin";
      break;
    case "threads":
      ui.activePage.value = "threads";
      break;
    case "unread":
      ui.activePage.value = "unread";
      break;
  }
  ui.activeCommunitySurface.value =
    match.id === "feed" || match.id === "stories" || match.id === "events"
      ? match.id === "stories" ? "feed" : match.id
      : null;
  if (match.id === "stories") {
    ui.feedDefaultFilter.value = "stories";
    ui.feedDefaultComposerMode.value = "story";
  } else if (match.id === "feed") {
    ui.feedDefaultFilter.value = "all";
    ui.feedDefaultComposerMode.value = "post";
  }
  ui.sidebarMode.value =
    match.id === "dm" || match.id === "groupDmRoom" || match.id === "dmList" ? "dms" : "channels";
  // #414/#951: channel and DM conversation routes carry the pinned-panel flag.
  ui.showPinnedPanel.value =
    match.id === "channel" || match.id === "channelExtension" || match.id === "dm" || match.id === "groupDmRoom"
      ? match.search.pinned
      : false;
}

interface RouteSyncDeps {
  ui: ChatShellState;
  session: ComputedRef<WaddleSession | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  isApplyingRoute: Ref<boolean>;
  activeDmPeer: ComputedRef<{ peerJid: string; peerUsername: string } | null>;
  activeThreadStack: Ref<string[]>;
  activeThreadTargetMessageId: Ref<string | null>;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  clearPendingChannelRoomJidSelection: () => void;
  openDm: (peerJid: string) => Promise<void>;
  selectGroupDm: (
    roomJid: string,
    options?: { updateUrl?: boolean; intent?: ChannelLoadIntent },
  ) => Promise<boolean>;
  /** Owned by the connection lifecycle's structure-retry state: a user
   * navigation supersedes any channel route parked for a structure reload. */
  clearPendingChannelRoute: () => void;
}

/**
 * URL <-> shell-state synchronisation: `updateUrl` derives the canonical
 * URL from controller state after every relevant mutation, and
 * `applyRouteTarget` is the single state-from-match handler covering
 * every route id (deep links, back/forward, and connection bootstrap).
 */
export function useRouteSync(deps: RouteSyncDeps) {
  const {
    ui,
    session,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    isApplyingRoute,
    activeDmPeer,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeRightPanel,
    activeExtensionRouteKey,
    clearPendingChannelRoomJidSelection,
    openDm,
    selectGroupDm,
    clearPendingChannelRoute,
  } = deps;

  let routeRequestId = 0;

  /** Start a new route application; any in-flight `applyRouteTarget` with
   * an older id becomes stale and stops mutating state. */
  function beginRouteRequest(): number {
    return ++routeRequestId;
  }

  function isCurrentRouteRequest(requestId: number): boolean {
    return requestId === routeRequestId;
  }

  function updateUrl() {
    if (isApplyingRoute.value) return;
    // Community-surface routes win over the page-switch ladder: a
    // watcher firing while activeCommunitySurface is set must not
    // bounce the URL back to channel/home before the surface clears.
    const surface = ui.activeCommunitySurface.value;
    if (surface === "feed" || surface === "events") {
      navigate({ id: surface });
      return;
    }
    if (ui.activePage.value === "settings") {
      navigate({ id: "settings", origin: "app" });
      return;
    }
    if (ui.activePage.value === "threads") {
      // Without this branch the fall-through below would push the
      // home route ("/") every time a watcher fires after
      // `openThreads()` — bouncing the URL off /threads immediately,
      // and reverting /threads back to / on initial load when
      // `onConnectionReady`'s `finally` calls updateUrl.
      navigate({ id: "threads" });
      return;
    }
    if (ui.activePage.value === "unread") {
      // Mirror the threads branch: keep the URL pinned to /unread while
      // watchers fire, instead of falling through to the home route.
      navigate({ id: "unread" });
      return;
    }
    if (ui.activePage.value === "admin") {
      // Admin owns the entire workspace and is mounted out of
      // ChatReadyShell's own popstate-driven adminPanelRef, but
      // updateUrl still needs an admin-aware branch so a stray
      // activePage watcher doesn't drag the user back to /.
      return;
    }
    if (ui.activePage.value === "dashboard") {
      navigate({ id: "home" });
      return;
    }
    const channel = waddles.currentChannel.value;
    if (ui.activePage.value === "chat" && activeRightPanel.value === "extension") {
      const ext = activeExtensionRouteKey.value;
      if (channel && ext) {
        navigate({
          id: "channelExtension",
          params: { channelId: channel.id, pluginId: ext.pluginId, routeId: ext.routeId },
          search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
        });
      } else {
        navigate({ id: "home" });
      }
      return;
    }
    if (ui.sidebarMode.value === "dms") {
      if (activeDmPeer.value) {
        navigate({
          id: "dm",
          params: { username: activeDmPeer.value.peerUsername },
          search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
        });
      } else if (channel?.isGroupDm && channel.jid) {
        navigate({
          id: "groupDmRoom",
          params: { roomJid: channel.jid },
          search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
        });
      } else {
        navigate({ id: "dmList" });
      }
    } else if (channel) {
      navigate({
        id: "channel",
        params: { channelId: channel.id },
        search: { thread: activeThreadStack.value, pinned: ui.showPinnedPanel.value },
      });
    } else {
      navigate({ id: "home" });
    }
  }

  function onPopState() {
    // applyRouteTarget is the single state-from-match handler — it
    // covers every route id and is the only place that mutates
    // controller state in response to a URL change. onPopState's job
    // is just to invoke it with the new URL and manage the
    // `isApplyingRoute` lifecycle.
    const match = matchLocation(window.location.pathname, window.location.search);
    clearPendingChannelRoute();
    const requestId = ++routeRequestId;
    isApplyingRoute.value = true;
    void applyRouteTarget(match, requestId, {
      intent: "explicit-navigation",
    }).finally(() => {
      if (requestId === routeRequestId) {
        isApplyingRoute.value = false;
      }
    });
  }

  async function applyRouteTarget(
    match: RouteMatch,
    requestId: number,
    options: { intent?: ChannelLoadIntent } = {},
  ) {
    clearPendingChannelRoomJidSelection();
    applyMatchToShellState(ui, match);
    if (match.id === "stories") {
      navigate({ id: "feed" }, { replace: true });
    }
    // Routes that don't reference a specific channel/DM target also
    // drop the chat-context state. (Channel/extension/dm routes set
    // their own context further down.)
    if (
      match.id === "home"
      || match.id === "feed"
      || match.id === "stories"
      || match.id === "events"
      || match.id === "dmList"
    ) {
      dmConversations.closeDm();
      waddles.activeChannelId.value = null;
      activeExtensionRouteKey.value = null;
      activeRightPanel.value = null;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      if (match.id === "home") messaging.clearMessages();
      return;
    }
    if (
      match.id === "admin"
      || match.id === "settings"
      || match.id === "threads"
      || match.id === "unread"
    ) {
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      activeExtensionRouteKey.value = null;
      return;
    }
    if (match.id === "dm") {
      activeExtensionRouteKey.value = null;
      const username = match.params.username.replace(/^@/, "").trim();
      if (username) {
        const domain = session.value ? jidDomain(session.value.jid) : "";
        if (!domain) return;
        await openDm(`${username}@${domain}`);
        if (requestId !== routeRequestId) return;
      }
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      ui.showPinnedPanel.value = match.search.pinned;
      if (match.search.pinned) {
        activeRightPanel.value = "pinned";
      } else if (match.search.thread.length > 0) {
        activeRightPanel.value = "thread";
      } else {
        activeRightPanel.value = null;
      }
      // Dedup: a nested stack can legitimately repeat a thread id (e.g.
      // `[A,B,A]`), but each thread only needs one backfill.
      for (const threadId of new Set(match.search.thread)) {
        void dmMessaging.backfillThread(threadId);
      }
      return;
    }

    if (match.id === "groupDmRoom") {
      activeExtensionRouteKey.value = null;
      dmConversations.closeDm();
      await selectGroupDm(match.params.roomJid, {
        updateUrl: false,
        intent: options.intent ?? "automatic",
      });
      if (requestId !== routeRequestId) return;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      ui.showPinnedPanel.value = match.search.pinned;
      activeRightPanel.value = match.search.pinned
        ? "pinned"
        : match.search.thread.length > 0
          ? "thread"
          : null;
      for (const threadId of new Set(match.search.thread)) {
        void messaging.backfillThread(threadId);
      }
      return;
    }


    if (match.id === "channelExtension") {
      ui.sidebarMode.value = "channels";
      dmConversations.closeDm();
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      const ch = resolveChannelBySlug(match.params.channelId, waddles.channels.value);
      if (!ch) {
        waddles.activeChannelId.value = null;
        activeExtensionRouteKey.value = null;
        return;
      }
      waddles.activeChannelId.value = ch.id;
      void waddles.reloadChannelMembers(ch.id);
      const nextExtensionRouteKey = {
        channelId: ch.id,
        pluginId: match.params.pluginId,
        routeId: match.params.routeId,
      };
      messaging.clearMessages();
      await messaging.loadMessages(ch.spaceId ?? "", ch.id, 0, [], {
        intent: options.intent ?? "automatic",
      });
      if (requestId !== routeRequestId) return;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      ui.showPinnedPanel.value = match.search.pinned;
      for (const threadId of match.search.thread) {
        void messaging.backfillThread(threadId);
      }
      activeExtensionRouteKey.value = nextExtensionRouteKey;
      activeRightPanel.value = "extension";
      return;
    }

    // match.id === "channel"
    activeExtensionRouteKey.value = null;
    const ch = resolveChannelBySlug(match.params.channelId, waddles.channels.value);
    if (!ch) {
      waddles.activeChannelId.value = null;
      messaging.clearMessages();
      return;
    }
    if (ch.isGroupDm && ch.jid) {
      navigate({
        id: "groupDmRoom",
        params: { roomJid: ch.jid },
        search: match.search,
      }, { replace: true });
      await selectGroupDm(ch.jid, {
        updateUrl: false,
        intent: options.intent ?? "automatic",
      });
      ui.showPinnedPanel.value = match.search.pinned;
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = match.search.thread;
      activeRightPanel.value = match.search.pinned
        ? "pinned"
        : match.search.thread.length > 0
          ? "thread"
          : null;
      for (const threadId of match.search.thread) {
        void messaging.backfillThread(threadId);
      }
      return;
    }
    waddles.activeChannelId.value = ch.id;
    void waddles.reloadChannelMembers(ch.id);
    messaging.clearMessages();
    await messaging.loadMessages(ch.spaceId ?? "", ch.id, 0, [], {
      intent: options.intent ?? "automatic",
    });

    // Restore the thread panel from the URL and initialize paging for every
    // visible thread pane. Dedupe in the messaging composable keeps already
    // loaded roots/replies stable.
    ui.showPinnedPanel.value = match.search.pinned;
    activeThreadTargetMessageId.value = null;
    activeThreadStack.value = match.search.thread;
    if (match.search.pinned) {
      activeRightPanel.value = "pinned";
    } else if (match.search.thread.length > 0) {
      activeRightPanel.value = "thread";
    } else {
      activeRightPanel.value = null;
    }
    for (const threadId of match.search.thread) {
      void messaging.backfillThread(threadId);
    }
  }

  onMounted(() => {
    window.addEventListener("popstate", onPopState);
  });

  onUnmounted(() => {
    window.removeEventListener("popstate", onPopState);
  });

  return {
    beginRouteRequest,
    isCurrentRouteRequest,
    updateUrl,
    applyRouteTarget,
  };
}
