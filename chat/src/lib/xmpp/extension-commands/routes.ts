import type { ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import { jidDomain } from "../jid";
import type {
  DiscoveredExtensionRoute,
  ExtensionRouteItem,
  XmppExtensionRoutes,
} from "./types";

function normalizeExtensionRoute(value: unknown): DiscoveredExtensionRoute | null {
  const route = value as Record<string, unknown>;
  const serviceJid = stringField(route, "serviceJid") ?? stringField(route, "service_jid");
  const pluginId = stringField(route, "pluginId") ?? stringField(route, "plugin_id");
  const routeId = stringField(route, "routeId") ?? stringField(route, "route_id");
  const label = stringField(route, "label");
  const scope = stringField(route, "scope");
  const surface = stringField(route, "surface");
  const stateNode = stringField(route, "stateNode") ?? stringField(route, "state_node");
  const payloadNamespace = stringField(route, "payloadNamespace") ?? stringField(route, "payload_namespace");
  if (
    !serviceJid
    || !pluginId
    || !routeId
    || !label
    || scope !== "channel"
    || (surface !== "gallery" && surface !== "list")
    || !stateNode
    || !payloadNamespace
  ) {
    return null;
  }
  return {
    serviceJid,
    pluginId,
    routeId,
    label,
    scope,
    surface,
    stateNode,
    payloadNamespace,
  };
}

function normalizeExtensionRouteItem(value: unknown): ExtensionRouteItem | null {
  const item = value as Record<string, unknown>;
  const fields = arrayField(item, "fields").flatMap((field) => {
    const value = field as Record<string, unknown>;
    const name = stringField(value, "name");
    const fieldValue = stringField(value, "value");
    if (!name || fieldValue === null) return [];
    const label = stringField(value, "label");
    return label ? [{ name, label, value: fieldValue }] : [{ name, value: fieldValue }];
  });
  const options = arrayField(item, "options").flatMap((option) => {
    const value = option as Record<string, unknown>;
    const id = stringField(value, "id");
    const label = stringField(value, "label");
    return id && label ? [{ id, label }] : [];
  });
  const actions = arrayField(item, "actions").flatMap((action) => {
    const value = action as Record<string, unknown>;
    const launchValue = value.launch && typeof value.launch === "object"
      ? value.launch as Record<string, unknown>
      : value;
    const launchId = stringField(value, "launchId") ?? stringField(value, "launch_id") ?? stringField(launchValue, "id");
    const label = stringField(value, "label") ?? stringField(launchValue, "label");
    if (!launchId || !label) return [];
    const launch = routeItemLaunchDescriptor(launchValue, launchId, label);
    return launch ? [{ launchId, label, launch }] : [];
  });
  return {
    ...(stringField(item, "id") ? { id: stringField(item, "id")! } : {}),
    ...(stringField(item, "title") ? { title: stringField(item, "title")! } : {}),
    ...(stringField(item, "subtitle") ? { subtitle: stringField(item, "subtitle")! } : {}),
    ...(stringField(item, "description") ? { description: stringField(item, "description")! } : {}),
    ...(linkField(item) ? { link: linkField(item)! } : {}),
    fields,
    options,
    actions,
  };
}

function routeItemLaunchDescriptor(
  value: Record<string, unknown>,
  launchId: string,
  label: string,
): ExtensionLaunchDescriptor | null {
  const pluginId = stringField(value, "pluginId") ?? stringField(value, "plugin_id") ?? stringField(value, "plugin");
  const actionId = stringField(value, "actionId") ?? stringField(value, "action_id") ?? stringField(value, "action");
  const commandNode = stringField(value, "commandNode") ?? stringField(value, "command_node") ?? stringField(value, "command-node");
  const launchToken = stringField(value, "launchToken") ?? stringField(value, "launch_token") ?? stringField(value, "token");
  const expiresAt = stringField(value, "expiresAt") ?? stringField(value, "expires_at") ?? stringField(value, "expires-at");
  const waddleId = stringField(value, "waddleId") ?? stringField(value, "waddle_id") ?? stringField(value, "waddle-id");
  if (!pluginId || !actionId || !commandNode || !launchToken || !expiresAt || !waddleId) return null;
  const roomJid = stringField(value, "roomJid") ?? stringField(value, "room_jid") ?? stringField(value, "room");
  const stanzaId = stringField(value, "sourceStanzaId") ?? stringField(value, "source_stanza_id") ?? stringField(value, "source-stanza-id");
  return {
    id: launchId,
    pluginId,
    actionId,
    commandNode,
    launchToken,
    expiresAt,
    label,
    context: {
      waddleId,
      ...(roomJid ? { roomJid } : {}),
      ...(stanzaId ? { stanzaId } : {}),
    },
    payloads: [],
  };
}

function stringField(value: Record<string, unknown>, field: string): string | null {
  const raw = value[field];
  return typeof raw === "string" && raw.trim() ? raw : null;
}

function arrayField(value: Record<string, unknown>, field: string): unknown[] {
  const raw = value[field];
  return Array.isArray(raw) ? raw : [];
}

function linkField(value: Record<string, unknown>): { href: string } | null {
  const link = value.link as Record<string, unknown> | undefined;
  const href = link ? stringField(link, "href") : null;
  return href ? { href } : null;
}

export async function discoverExtensionRoutes(
  xmpp: XmppExtensionRoutes,
  userJid: string,
): Promise<DiscoveredExtensionRoute[]> {
  if (!xmpp.discover_extension_routes) {
    throw new Error(`Rust extension route discovery is not available for ${jidDomain(userJid)}.`);
  }
  const routes = await xmpp.discover_extension_routes();
  return (Array.isArray(routes) ? routes : [])
    .map(normalizeExtensionRoute)
    .filter((route): route is DiscoveredExtensionRoute => !!route);
}

export function resolveExtensionRouteStateNode(route: DiscoveredExtensionRoute, roomJid: string): string {
  return route.stateNode.replaceAll("{room}", roomJid);
}

export async function fetchExtensionRouteItems(
  xmpp: XmppExtensionRoutes,
  route: DiscoveredExtensionRoute,
  roomJid: string,
): Promise<ExtensionRouteItem[]> {
  if (!xmpp.fetch_extension_route_items) {
    throw new Error("Rust extension route item loading is not available.");
  }
  const items = await xmpp.fetch_extension_route_items(route, roomJid);
  return (Array.isArray(items) ? items : [])
    .map(normalizeExtensionRouteItem)
    .filter((item): item is ExtensionRouteItem => !!item);
}
