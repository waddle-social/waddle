import { computed, type ComputedRef, type Ref } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";
import type { ActiveRightPanel } from "@/shell/controllers/use-thread-panels";

export interface ExtensionRouteKey {
  channelId: string;
  pluginId: string;
  routeId: string;
}

interface ExtensionRoutesDeps {
  ui: ChatShellState;
  xmppClient: ComputedRef<BrowserXmppClient | null>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  extensionRoutes: Ref<DiscoveredExtensionRoute[]>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  memberJidByNick: Ref<Record<string, string>>;
  clearPendingChannelRoomJidSelection: () => void;
  updateUrl: () => void;
}

/**
 * XEP-discovered extension routes: discovery refresh, the derived
 * active-route view state, and route selection (which also activates the
 * channel that hosts the route).
 */
export function useExtensionRoutes(deps: ExtensionRoutesDeps) {
  const {
    ui,
    xmppClient,
    waddles,
    messaging,
    dmConversations,
    extensionRoutes,
    activeExtensionRouteKey,
    activeRightPanel,
    memberJidByNick,
    clearPendingChannelRoomJidSelection,
    updateUrl,
  } = deps;

  const activeExtensionRoute = computed(() => {
    const key = activeExtensionRouteKey.value;
    if (!key) return null;
    return extensionRoutes.value.find((route) =>
      route.pluginId === key.pluginId && route.routeId === key.routeId,
    ) ?? null;
  });
  const channelExtensionRoutes = computed(() =>
    ui.sidebarMode.value === "channels" && waddles.currentChannel.value
      ? extensionRoutes.value.filter((route) => route.scope === "channel")
      : [],
  );

  async function refreshExtensionRoutes() {
    if (!xmppClient.value) {
      extensionRoutes.value = [];
      return;
    }
    try {
      extensionRoutes.value = await xmppClient.value.discoverExtensionRoutes();
    } catch (error) {
      console.warn("Unable to discover extension routes", error);
      extensionRoutes.value = [];
    }
  }

  async function selectExtensionRoute(channelId: string, route: DiscoveredExtensionRoute) {
    clearPendingChannelRoomJidSelection();
    ui.activePage.value = "chat";
    ui.sidebarMode.value = "channels";
    dmConversations.closeDm();
    memberJidByNick.value = {};
    if (waddles.activeChannelId.value !== channelId) {
      waddles.activeChannelId.value = channelId;
      messaging.clearMessages();
      await messaging.loadMessages(
        waddles.currentChannel.value?.spaceId ?? "",
        channelId,
        0,
        [],
        { allowAccessRetry: true },
      );
    }
    activeExtensionRouteKey.value = { channelId, pluginId: route.pluginId, routeId: route.routeId };
    activeRightPanel.value = "extension";
    updateUrl();
    void waddles.reloadChannelMembers(channelId);
    ui.showMobileNav.value = false;
  }

  return {
    activeExtensionRoute,
    channelExtensionRoutes,
    refreshExtensionRoutes,
    selectExtensionRoute,
  };
}
