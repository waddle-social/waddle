import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { extensionRouteRailItems } from "../src/components/chat/extension-route-rail-model";
import type { DiscoveredExtensionRoute } from "../src/lib/xmpp/extension-commands";

const routes: DiscoveredExtensionRoute[] = [
  {
    serviceJid: "extensions.waddle.test",
    pluginId: "link-board",
    routeId: "saved-links",
    label: "Saved Links",
    scope: "channel",
    surface: "list",
    stateNode: "urn:waddle:link-board",
    payloadNamespace: "urn:waddle:link-board:payload",
  },
  {
    serviceJid: "extensions.waddle.test",
    pluginId: "polls",
    routeId: "active-polls",
    label: "Polls",
    scope: "channel",
    surface: "gallery",
    stateNode: "urn:waddle:polls",
    payloadNamespace: "urn:waddle:polls:payload",
  },
];

describe("extension route rail UI contract", () => {
  test("keeps extension routes out of the channel list", () => {
    const topicsPanel = readFileSync(new URL("../src/components/chat/TopicsPanel.vue", import.meta.url), "utf8");

    expect(topicsPanel).not.toContain("channelExtensionRoutes");
    expect(topicsPanel).not.toContain("selectExtensionRoute");
    expect(topicsPanel).not.toContain("extension route");
  });

  test("builds rail button state from discovered extension routes", () => {
    const items = extensionRouteRailItems(routes, { pluginId: "polls", routeId: "active-polls" }, true);

    expect(items).toEqual([
      {
        key: "link-board:saved-links",
        label: "Saved Links",
        icon: "links",
        isActive: false,
        route: routes[0],
      },
      {
        key: "polls:active-polls",
        label: "Polls",
        icon: "gallery",
        isActive: true,
        route: routes[1],
      },
    ]);
  });

  test("does not mark a route active when the extension panel is collapsed", () => {
    const items = extensionRouteRailItems(routes, { pluginId: "polls", routeId: "active-polls" }, false);

    expect(items.map((item) => item.isActive)).toEqual([false, false]);
  });

  test("keeps direct extension routes in chat with a right-side panel", () => {
    const readyShell = readFileSync(new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url), "utf8");
    const controller = readFileSync(new URL("../src/shell/controllers/use-route-sync.ts", import.meta.url), "utf8");
    const state = readFileSync(new URL("../src/shell/state.ts", import.meta.url), "utf8");

    expect(state).toContain('ref<"dashboard" | "chat" | "settings" | "admin" | "threads" | "unread">');
    expect(readyShell).toContain("<ExtensionRouteRail");
    expect(readyShell).toContain("@close=\"closeExtensionRoutePanel\"");
    expect(readyShell).toContain("@update:pinned-panel-open=\"setPinnedPanelOpen\"");
    expect(readyShell).toContain("chat-workspace--right-panel-active");
    expect(readyShell).not.toContain("v-else-if=\"ui.activePage.value === 'extension'\"");

    const rail = readFileSync(new URL("../src/components/chat/ExtensionRouteRail.vue", import.meta.url), "utf8");
    const routeView = readFileSync(new URL("../src/components/chat/ExtensionRouteView.vue", import.meta.url), "utf8");
    expect(rail).toContain(":aria-current=\"item.isActive ? 'page' : undefined\"");
    expect(rail).not.toContain(":aria-pressed");
    expect(routeView).toContain(":aria-label=\"route?.label ? `${route.label} extension route` : 'Extension route'\"");
    expect(routeView).toContain('ref="panelRef"');
    expect(routeView).toContain('tabindex="-1"');
    expect(routeView).not.toContain("<main");
    expect(routeView).toContain("type-caption inline-flex min-h-8");
    expect(controller).toContain("applyMatchToShellState(ui, match)");
    const extensionStart = controller.indexOf('if (match.id === "channelExtension") {');
    const loadIndex = controller.indexOf("await messaging.loadMessages(", extensionStart);
    const guardIndex = controller.indexOf("if (requestId !== routeRequestId) return;", loadIndex);
    const pinnedIndex = controller.indexOf("ui.showPinnedPanel.value = match.search.pinned;", guardIndex);
    const panelIndex = controller.indexOf('activeRightPanel.value = "extension";', pinnedIndex);
    expect(extensionStart).toBeGreaterThan(-1);
    expect(loadIndex).toBeGreaterThan(extensionStart);
    expect(guardIndex).toBeGreaterThan(loadIndex);
    expect(pinnedIndex).toBeGreaterThan(guardIndex);
    expect(panelIndex).toBeGreaterThan(pinnedIndex);
  });
});
