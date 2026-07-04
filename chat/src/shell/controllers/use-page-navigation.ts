import type { ComputedRef, Ref } from "vue";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import { navigate } from "@/router";
import type { ActiveRightPanel } from "@/shell/controllers/use-thread-panels";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";

interface PageNavigationDeps {
  ui: ChatShellState;
  waddles: ReturnType<typeof useWaddleDirectory>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  activeDmPeer: ComputedRef<{ peerJid: string } | null>;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  activeThreadStack: Ref<string[]>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  clearPendingChannelRoomJidSelection: () => void;
  updateUrl: () => void;
}

/**
 * Top-level page switches (home / DM list / threads / unread / community
 * surfaces / settings): each mutates the in-page shell state up front so
 * ChatReadyShell's v-else-if cascade re-renders this tick, then pushes
 * the URL so refresh and back/forward land on the same place.
 */
export function usePageNavigation(deps: PageNavigationDeps) {
  const {
    ui,
    waddles,
    dmConversations,
    activeDmPeer,
    activeRightPanel,
    activeThreadStack,
    activeExtensionRouteKey,
    clearPendingChannelRoomJidSelection,
    updateUrl,
  } = deps;

  function openUserSettings() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "settings";
  }

  /**
   * Navigate to the global Threads view. Clears any channel/DM
   * selection, drops local thread-panel state, syncs the URL so a
   * page refresh lands back on /threads.
   */
  function openThreads() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "threads";
    // ChatReadyShell renders FeedPane / EventsPane on
    // `activeCommunitySurface` v-else-if branches BEFORE the threads
    // branch — leaving the surface non-null would let one of those win
    // and ThreadsView would silently not render even though the URL
    // changed. Clear the surface so the threads v-else-if matches.
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "threads" });
  }

  /**
   * Navigate to the global Unread view. Mirrors `openThreads()`: clears
   * any channel/DM selection and local thread-panel state, clears the
   * active community surface so the `unread` v-else-if branch in
   * ChatReadyShell wins, and syncs the URL so a refresh lands on /unread.
   */
  function openUnread() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "unread";
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "unread" });
  }

  /**
   * Navigate to a community surface — Feed / Events. These
   * are first-class routes (`/feed`, `/events`) but they
   * also need the in-page state set immediately so the v-else-if
   * branches in ChatReadyShell re-render this tick. `pushState` (via
   * `navigate()`) doesn't fire `popstate`, so the controller's
   * popstate-driven state sync only covers back/forward; in-app
   * clicks need to mirror what `applyRouteTarget` would do.
   */
  function openCommunitySurface(surface: "feed" | "events") {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "chat";
    ui.activeCommunitySurface.value = surface;
    if (surface === "feed") {
      ui.feedDefaultFilter.value = "all";
      ui.feedDefaultComposerMode.value = "post";
    }
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    waddles.activeChannelId.value = null;
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: surface });
  }

  /**
   * Navigate back to the Home dashboard from anywhere. Clears any
   * channel/DM selection, drops thread state, closes the mobile nav
   * drawer, and resyncs the URL so a refresh returns the user to home
   * rather than re-entering the last channel.
   */
  function openHome() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "dashboard";
    ui.sidebarMode.value = "channels";
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "home" });
  }

  /**
   * Navigate to the DM list (`/dm`). Mirrors `openHome` / `openThreads`:
   * mutates the in-page state up front so the v-else-if cascade in
   * ChatReadyShell switches to DM mode this tick, then pushes the URL
   * so refresh and back/forward land on the same place.
   */
  function openDmList() {
    clearPendingChannelRoomJidSelection();
    ui.showMobileNav.value = false;
    ui.showMobileDetails.value = false;
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "dms";
    ui.activeCommunitySurface.value = null;
    waddles.activeChannelId.value = null;
    dmConversations.closeDm();
    activeRightPanel.value = null;
    activeThreadStack.value = [];
    activeExtensionRouteKey.value = null;
    navigate({ id: "dmList" });
  }

  function closeUserSettings() {
    const state = window.history.state as { waddleRouteId?: string; origin?: string } | null;
    if (
      window.location.pathname === "/settings"
      && state?.waddleRouteId === "settings"
      && state.origin === "app"
    ) {
      window.history.back();
      return;
    }
    // Set the page we want to land on; updateUrl() reads the rest of
    // the controller state and constructs the right typed match
    // (channel / extension / DM / home / community surface).
    ui.activePage.value = waddles.currentChannel.value || activeDmPeer.value
      ? "chat"
      : "dashboard";
    updateUrl();
  }

  return {
    openUserSettings,
    openThreads,
    openUnread,
    openCommunitySurface,
    openHome,
    openDmList,
    closeUserSettings,
  };
}
