import { Grid3X3, Link2, ListChecks } from "lucide-vue-next";
import type { Component } from "vue";
import type { DiscoveredExtensionRoute } from "@/lib/xmpp/extension-commands";

type ExtensionRouteRailIcon = "gallery" | "links" | "list";

export function extensionRouteIconComponent(icon: ExtensionRouteRailIcon): Component {
  if (icon === "links") return Link2;
  if (icon === "gallery") return Grid3X3;
  return ListChecks;
}

interface ExtensionRouteRailItem {
  key: string;
  label: string;
  icon: ExtensionRouteRailIcon;
  isActive: boolean;
  route: DiscoveredExtensionRoute;
}

interface ActiveExtensionRouteKey {
  pluginId: string;
  routeId: string;
}

function routeIcon(route: DiscoveredExtensionRoute): ExtensionRouteRailIcon {
  if (route.routeId.includes("link")) return "links";
  if (route.surface === "gallery") return "gallery";
  return "list";
}

export function extensionRouteRailItems(
  routes: DiscoveredExtensionRoute[],
  activeRoute: ActiveExtensionRouteKey | null | undefined,
  isRailActive = false,
): ExtensionRouteRailItem[] {
  return routes.map((route) => ({
    key: `${route.pluginId}:${route.routeId}`,
    label: route.label,
    icon: routeIcon(route),
    isActive: isRailActive
      && activeRoute?.pluginId === route.pluginId
      && activeRoute?.routeId === route.routeId,
    route,
  }));
}
