import type { ExtensionLaunchDescriptor } from "@/lib/chat-ui";

export interface XmppSendIq {
  send_raw_iq?(xml: string): Promise<string>;
  getDiscoItems?(jid: string, node?: string): Promise<{ items?: DiscoItem[] }>;
  getDiscoInfo?(jid: string, node?: string): Promise<{ features?: string[]; extensions?: unknown[] }>;
  getItems?(jid: string, node: string, opts?: { max?: number }): Promise<{ items?: Array<{ id?: string; content?: unknown }> }>;
}

export interface XmppExtensionRoutes {
  discover_extension_routes?: () => Promise<unknown>;
  fetch_extension_route_items?: (route: unknown, roomJid: string) => Promise<unknown>;
}

export interface DiscoItem {
  jid?: string;
  node?: string;
  name?: string;
}

export interface ExtensionCommandNote {
  type?: string;
  value: string;
}

export interface ExtensionCommandResult {
  status?: string;
  sessionId?: string;
  actions?: ExtensionCommandActions;
  notes: ExtensionCommandNote[];
  form?: unknown;
}

export type ExtensionCommandAction = "next" | "prev" | "complete" | "cancel";

export interface ExtensionCommandActions {
  execute?: ExtensionCommandAction;
  allowed: ExtensionCommandAction[];
}

export interface ExtensionCommandFormOption {
  label: string;
  value: string;
}

export interface ExtensionCommandFormField {
  name: string;
  label: string;
  type: string;
  description?: string;
  value: string;
  values: string[];
  options: ExtensionCommandFormOption[];
  required: boolean;
  blocked: boolean;
  hidden: boolean;
}

export type ExtensionCommandScope = "global" | "channel";

export interface DiscoveredExtensionCommand {
  serviceJid: string;
  node: string;
  name: string;
  scope: ExtensionCommandScope;
  composerPrefix?: string;
  inlineField?: string;
}

type ExtensionRouteScope = "channel";
type ExtensionRouteSurface = "gallery" | "list";

export interface DiscoveredExtensionRoute {
  serviceJid: string;
  pluginId: string;
  routeId: string;
  label: string;
  scope: ExtensionRouteScope;
  surface: ExtensionRouteSurface;
  stateNode: string;
  payloadNamespace: string;
}

interface ExtensionItemField {
  name: string;
  label?: string;
  value: string;
}

interface ExtensionItemOption {
  id: string;
  label: string;
}

interface ExtensionItemAction {
  launchId: string;
  label: string;
  launch?: ExtensionLaunchDescriptor;
}

export interface ExtensionRouteItem {
  id?: string;
  title?: string;
  subtitle?: string;
  link?: { href: string };
  description?: string;
  fields: ExtensionItemField[];
  options: ExtensionItemOption[];
  actions: ExtensionItemAction[];
}

type ExtensionCommandOutcomeState = "success" | "warning" | "error";

export interface ExtensionCommandOutcome {
  state: ExtensionCommandOutcomeState;
  detail?: string;
}

export interface DataFormField {
  name: string;
  type?: string;
  value: string | string[] | boolean;
}

export interface FormFieldLike {
  name?: unknown;
  var?: unknown;
  type?: unknown;
  value?: unknown;
  values?: unknown[];
  rawValues?: unknown[];
}

export interface ExtensionInvokeIq {
  type: "set";
  to: string;
  command: {
    node: string;
    action: "execute" | ExtensionCommandAction;
    form?: {
      type: "submit";
      fields: DataFormField[];
    };
  };
}
