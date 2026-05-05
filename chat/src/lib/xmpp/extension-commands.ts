import type { ExtensionAnnotationAction, ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import { jidDomain } from "./jid";

interface XmppSendIq {
  send_raw_iq?(xml: string): Promise<string>;
  getDiscoItems?(jid: string, node?: string): Promise<{ items?: Array<{ jid?: string; node?: string; name?: string }> }>;
  getDiscoInfo?(jid: string, node?: string): Promise<{ features?: string[]; extensions?: unknown[] }>;
  getItems?(jid: string, node: string, opts?: { max?: number }): Promise<{ items?: Array<{ id?: string; content?: unknown }> }>;
}

function requireRawIq(xmpp: XmppSendIq): (xml: string) => Promise<string> {
  if (typeof xmpp.send_raw_iq !== "function") {
    throw new Error("XMPP raw IQ sender is unavailable.");
  }
  return xmpp.send_raw_iq.bind(xmpp);
}

interface XmppExtensionRoutes {
  discover_extension_routes?: () => Promise<unknown>;
  fetch_extension_route_items?: (route: unknown, roomJid: string) => Promise<unknown>;
}

function escapeXml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
}
const NS_WADDLE_EXTENSION_1 = "urn:waddle:extension:1";
const NS_ADHOC_COMMANDS = "http://jabber.org/protocol/commands";
const INVOKE_COMMAND_NODE = "urn:waddle:extension:1:invoke";
const EXTENSION_COMMAND_FORM_TYPE = "urn:waddle:extension:1:command";


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
}

export type ExtensionRouteScope = "channel";
export type ExtensionRouteSurface = "gallery" | "list";

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

export interface ExtensionItemField {
  name: string;
  label?: string;
  value: string;
}

export interface ExtensionItemOption {
  id: string;
  label: string;
}

export interface ExtensionItemAction {
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

type ExtensionCommandOutcomeState = "success" | "warning" | "error";

interface ExtensionCommandOutcome {
  state: ExtensionCommandOutcomeState;
  detail?: string;
}

interface DataFormField {
  name: string;
  type?: string;
  value: string | string[] | boolean;
}

interface FormFieldLike {
  name?: unknown;
  var?: unknown;
  type?: unknown;
  value?: unknown;
  values?: unknown[];
  rawValues?: unknown[];
}

interface ExtensionInvokeIq {
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


export function extensionServiceJidForUserJid(userJid: string): string {
  const domain = jidDomain(userJid);
  if (!domain) throw new Error("Cannot resolve extension service for this XMPP account.");
  return `extensions.${domain}`;
}

function requiredLaunchValue(value: string | undefined, label: string): string {
  if (value?.trim()) return value;
  throw new Error(`Launch is missing ${label}.`);
}

export function buildExtensionLaunchInvokeIq(
  userJid: string,
  launch: ExtensionLaunchDescriptor,
  serviceJid = extensionServiceJidForUserJid(userJid),
): ExtensionInvokeIq {
  const messageStanzaId = launch.context.stanzaId ?? launch.source?.stanzaId;
  const fields: DataFormField[] = [
    { name: "FORM_TYPE", type: "hidden", value: NS_WADDLE_EXTENSION_1 },
    { name: "waddle#op", value: "invoke" },
    { name: "plugin", value: requiredLaunchValue(launch.pluginId, "plugin id") },
    { name: "action", value: requiredLaunchValue(launch.actionId, "action id") },
    { name: "waddle-id", value: requiredLaunchValue(launch.context.waddleId, "waddle id") },
    ...(messageStanzaId ? [{ name: "source-stanza-id", value: messageStanzaId }] : []),
    { name: "launch-id", value: requiredLaunchValue(launch.id, "launch id") },
    { name: "launch-token", value: requiredLaunchValue(launch.launchToken, "launch token") },
    { name: "expires-at", value: requiredLaunchValue(launch.expiresAt, "expiry") },
    { name: "waddle#waddle_id", value: requiredLaunchValue(launch.context.waddleId, "waddle id") },
    ...(messageStanzaId ? [{ name: "waddle#message_stanza_id", value: messageStanzaId }] : []),
    { name: "waddle#launch_id", value: requiredLaunchValue(launch.id, "launch id") },
    { name: "waddle#launch_token", value: requiredLaunchValue(launch.launchToken, "launch token") },
    { name: "waddle#expires_at", value: requiredLaunchValue(launch.expiresAt, "expiry") },
  ];
  if (launch.context.roomJid) {
    fields.splice(3, 0, { name: "waddle#room_jid", value: launch.context.roomJid });
  }
  for (const payload of launch.payloads) {
    fields.push(...payloadFields(payload));
  }
  return {
    type: "set",
    to: serviceJid,
    command: {
      node: INVOKE_COMMAND_NODE,
      action: "execute",
      form: {
        type: "submit",
        fields,
      },
    },
  };
}

function payloadFields(payload: ExtensionLaunchDescriptor["payloads"][number]): DataFormField[] {
  const prefix = `payload#${payload.name}`;
  return [
    ...Object.entries(payload.attributes)
      .filter(([name]) => name !== "xmlns")
      .map(([name, value]) => ({
        name: `${prefix}#${name}`,
        value,
      })),
    ...(payload.text ? [{ name: prefix, value: payload.text }] : []),
  ];
}

export function parseExtensionCommandResult(response: unknown): ExtensionCommandResult {
  const command = (response as { command?: { status?: unknown; sid?: unknown; sessionid?: unknown; sessionId?: unknown; availableActions?: unknown; actions?: unknown; notes?: Array<{ type?: unknown; value?: unknown }>; form?: unknown } } | undefined)?.command;
  const notes = Array.isArray(command?.notes) ? command.notes : [];
  const rawActions = command?.availableActions ?? command?.actions;
  const actions = parseCommandActions(
    rawActions,
    typeof command?.status === "string" ? command.status : undefined,
    rawActions !== undefined,
  );
  return {
    ...(typeof command?.status === "string" && command.status ? { status: command.status } : {}),
    ...(typeof command?.sid === "string" && command.sid ? { sessionId: command.sid } : {}),
    ...(typeof command?.sessionid === "string" && command.sessionid ? { sessionId: command.sessionid } : {}),
    ...(typeof command?.sessionId === "string" && command.sessionId ? { sessionId: command.sessionId } : {}),
    ...(actions ? { actions } : {}),
    ...(command?.form ? { form: command.form } : {}),
    notes: notes
      .map((note) => ({
        ...(typeof note.type === "string" && note.type ? { type: note.type } : {}),
        value: typeof note.value === "string" ? note.value : "",
      }))
      .filter((note) => note.value.length > 0),
  };
}

export function parseExtensionCommandForm(form: unknown): ExtensionCommandFormField[] {
  const fields = (form as { fields?: unknown[] } | undefined)?.fields;
  if (!Array.isArray(fields)) return [];
  return fields.flatMap((field) => {
    const item = field as {
      name?: unknown;
      var?: unknown;
      label?: unknown;
      type?: unknown;
      desc?: unknown;
      description?: unknown;
      value?: unknown;
      values?: unknown[];
      rawValues?: unknown[];
      required?: unknown;
      options?: unknown[];
    };
    const name = typeof item.name === "string" ? item.name : typeof item.var === "string" ? item.var : "";
    const type = typeof item.type === "string" ? item.type : "text-single";
    if (!name && type !== "fixed") return [];
    const values = Array.isArray(item.value)
      ? item.value
      : Array.isArray(item.values)
        ? item.values
        : Array.isArray(item.rawValues)
          ? item.rawValues
          : item.value !== undefined
            ? [item.value]
            : [];
    const stringValues = values
      .filter((value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
      .map((value) => String(value));
    const fieldValues = type === "boolean" && stringValues.length === 0 ? ["0"] : stringValues;
    const fieldName = name || `fixed:${fieldValues.join("\n")}`;
    return [{
      name: fieldName,
      label: typeof item.label === "string" && item.label ? item.label : fieldName,
      type,
      ...(typeof item.desc === "string" && item.desc ? { description: item.desc } : {}),
      ...(typeof item.description === "string" && item.description ? { description: item.description } : {}),
      value: fieldValues[0] ?? "",
      values: fieldValues,
      options: parseFieldOptions(item.options),
      required: item.required === true,
      blocked: isForbiddenExtensionCommandField(name, type),
      hidden: type === "hidden",
    }];
  });
}

export function visibleExtensionCommandFields(fields: ExtensionCommandFormField[]): ExtensionCommandFormField[] {
  return fields.filter((field) => !field.hidden);
}

export function extensionCommandFormBlockedReason(fields: ExtensionCommandFormField[]): string | undefined {
  const blocked = fields.find((field) => field.blocked);
  if (!blocked) return undefined;
  return `Extension command form contains a forbidden field: ${blocked.label}.`;
}

export function parseExtensionCommandLaunches(form: unknown): ExtensionAnnotationAction[] {
  const fields = parseExtensionCommandForm(form);
  const byName = new Map(fields.map((field) => [field.name, field.value]));
  const count = Number.parseInt(byName.get("launch-count") ?? "0", 10);
  const actions: ExtensionAnnotationAction[] = [];
  for (let index = 0; index < count; index += 1) {
    const prefix = `launch#${index}`;
    const id = byName.get(`${prefix}#id`);
    const pluginId = byName.get(`${prefix}#plugin`);
    const actionId = byName.get(`${prefix}#action`);
    const commandNode = byName.get(`${prefix}#command-node`);
    const label = byName.get(`${prefix}#label`);
    const waddleId = byName.get(`${prefix}#waddle-id`);
    const launchToken = byName.get(`${prefix}#token`);
    const expiresAt = byName.get(`${prefix}#expires-at`);
    if (!id || !pluginId || !actionId || !commandNode || !label || !waddleId || !launchToken || !expiresAt) {
      continue;
    }
    actions.push({
      label,
      route: id,
      launch: {
        id,
        pluginId,
        actionId,
        commandNode,
        launchToken,
        expiresAt,
        label,
        context: {
          waddleId,
          ...(byName.get(`${prefix}#source-stanza-id`) ? { stanzaId: byName.get(`${prefix}#source-stanza-id`)! } : {}),
        },
        payloads: parseResultLaunchPayloads(byName, prefix),
      },
    });
  }
  return actions;
}

function parseResultLaunchPayloads(byName: Map<string, string>, launchPrefix: string): ExtensionLaunchDescriptor["payloads"] {
  const payloads: ExtensionLaunchDescriptor["payloads"] = [];
  for (let index = 0; ; index += 1) {
    const prefix = `${launchPrefix}#payload#${index}`;
    const namespace = byName.get(`${prefix}#namespace`);
    const name = byName.get(`${prefix}#name`);
    if (!namespace || !name) break;
    const attrPrefix = `${prefix}#attr#`;
    const attributes = Object.fromEntries(
      [...byName.entries()]
        .filter(([field]) => field.startsWith(attrPrefix))
        .map(([field, value]) => [field.slice(attrPrefix.length), value]),
    );
    payloads.push({
      namespace,
      name,
      attributes: { xmlns: namespace, ...attributes },
      ...(byName.get(`${prefix}#text`) ? { text: byName.get(`${prefix}#text`) } : {}),
      children: [],
    });
  }
  return payloads;
}

export function extensionCommandOutcome(result: unknown): ExtensionCommandOutcome {
  const parsed = isExtensionCommandResult(result) ? result : parseExtensionCommandResult(result);
  const errorDetail = commandNotesDetail(parsed.notes, ["error"]);
  if (errorDetail) return { state: "error", detail: errorDetail };

  const warningDetail = commandNotesDetail(parsed.notes, ["warn", "warning"]);
  if (warningDetail) return { state: "warning", detail: warningDetail };

  const status = parsed.status?.trim().toLowerCase();
  if (status === "executing") {
    if (parsed.form && parseExtensionCommandForm(parsed.form).length > 0) {
      return { state: "warning" };
    }
    return { state: "warning", detail: "Extension returned a form that this client cannot complete yet." };
  }
  if (status && status !== "completed" && status !== "complete") {
    if (status === "canceled" || status === "cancelled") return { state: "error", detail: "Command canceled." };
    return { state: "warning", detail: `Command returned status: ${parsed.status}.` };
  }

  const infoDetail = commandNotesDetail(parsed.notes, ["info"]);
  return infoDetail ? { state: "success", detail: infoDetail } : { state: "success" };
}

function isExtensionCommandResult(value: unknown): value is ExtensionCommandResult {
  return !!value
    && typeof value === "object"
    && "notes" in value
    && Array.isArray((value as ExtensionCommandResult).notes);
}

function commandNotesDetail(notes: ExtensionCommandNote[], types: string[]): string | undefined {
  const normalizedTypes = new Set(types.map((type) => type.toLowerCase()));
  const values = notes
    .filter((note) => {
      const type = note.type?.toLowerCase() ?? "info";
      return normalizedTypes.has(type);
    })
    .map((note) => note.value.trim())
    .filter((value) => value.length > 0);
  return values.length > 0 ? values.join(" ") : undefined;
}

function dataFieldXml(name: string, type: string | undefined, value: string | string[] | boolean): string {
  const typeAttr = type ? ` type="${escapeXml(type)}"` : "";
  const values = Array.isArray(value) ? value : [String(value)];
  return `<field var="${escapeXml(name)}"${typeAttr}>${values.map((v) => `<value>${escapeXml(String(v))}</value>`).join("")}</field>`;
}

function buildCommandIqXml(to: string, node: string, action: string, fields?: DataFormField[], sessionId?: string): string {
  const sessionAttr = sessionId ? ` sessionid="${escapeXml(sessionId)}"` : "";
  const formXml = fields?.length
    ? `<x xmlns="jabber:x:data" type="submit">${fields.map((f) => dataFieldXml(f.name, typeof f.value === "boolean" ? "boolean" : undefined, f.value)).join("")}</x>`
    : "";
  return `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(to)}"><command xmlns="${NS_ADHOC_COMMANDS}" node="${escapeXml(node)}" action="${escapeXml(action)}"${sessionAttr}>${formXml}</command></iq>`;
}

function decodeXml(text: string): string {
  return text
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", '"')
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

function readXmlAttr(attrs: string, name: string): string | undefined {
  const match = attrs.match(new RegExp(`${name}=["']([^"']*)["']`));
  return match?.[1] ? decodeXml(match[1]) : undefined;
}

function parseCommandIqResponse(xml: string): ExtensionCommandResult {
  const commandMatch = xml.match(/<command\b([^>]*)>([\s\S]*?)<\/command>|<command\b([^>]*)\/>/);
  const commandAttrs = commandMatch?.[1] ?? commandMatch?.[3] ?? "";
  if (!commandMatch) return { notes: [] };
  const commandBody = commandMatch[2] ?? "";
  const status = readXmlAttr(commandAttrs, "status");
  const sessionId = readXmlAttr(commandAttrs, "sessionid") ?? readXmlAttr(commandAttrs, "sid");
  const notes: ExtensionCommandNote[] = Array.from(commandBody.matchAll(/<note\b([^>]*)>([\s\S]*?)<\/note>/g))
    .map(([, attrs, value]) => ({ type: readXmlAttr(attrs, "type"), value: decodeXml(value).trim() }))
    .filter((note) => note.value.length > 0);
  const actionsMatch = commandBody.match(/<actions\b([^>]*)>([\s\S]*?)<\/actions>/);
  const actions = actionsMatch ? {
    execute: readXmlAttr(actionsMatch[1], "execute"),
    next: actionsMatch[2].includes("<next") || undefined,
    prev: actionsMatch[2].includes("<prev") || actionsMatch[2].includes("<previous") || undefined,
    complete: actionsMatch[2].includes("<complete") || undefined,
    cancel: actionsMatch[2].includes("<cancel") || undefined,
  } : undefined;
  const formMatch = commandBody.match(/<x\b([^>]*)>([\s\S]*?)<\/x>/);
  const form = formMatch ? {
    type: readXmlAttr(formMatch[1], "type") ?? "form",
    fields: Array.from(formMatch[2].matchAll(/<field\b([^>]*)>([\s\S]*?)<\/field>/g)).map(([, attrs, body]) => ({
      name: readXmlAttr(attrs, "var") ?? "",
      var: readXmlAttr(attrs, "var") ?? "",
      type: readXmlAttr(attrs, "type") ?? "text-single",
      label: readXmlAttr(attrs, "label"),
      desc: body.match(/<desc>([\s\S]*?)<\/desc>/)?.[1] ? decodeXml(body.match(/<desc>([\s\S]*?)<\/desc>/)![1]) : undefined,
      value: decodeXml(body.match(/<value>([\s\S]*?)<\/value>/)?.[1] ?? ""),
      values: Array.from(body.matchAll(/<value>([\s\S]*?)<\/value>/g)).map(([, value]) => decodeXml(value)),
      required: body.includes("<required"),
      options: Array.from(body.matchAll(/<option\b([^>]*)>([\s\S]*?)<\/option>/g)).map(([, optionAttrs, optionBody]) => ({
        label: readXmlAttr(optionAttrs, "label"),
        value: decodeXml(optionBody.match(/<value>([\s\S]*?)<\/value>/)?.[1] ?? ""),
      })),
    })),
  } : undefined;
  return {
    ...(status ? { status } : {}),
    ...(sessionId ? { sessionId } : {}),
    notes,
    ...(actions ? { actions: parseCommandActions(actions, status, !!actionsMatch) } : {}),
    ...(form ? { form } : {}),
  };
}

async function rawDiscoItems(xmpp: XmppSendIq, jid: string, node?: string): Promise<Array<{ jid?: string; node?: string; name?: string }>> {
  if (typeof xmpp.getDiscoItems === "function") {
    const result = await xmpp.getDiscoItems(jid, node);
    return result.items ?? [];
  }
  if (typeof xmpp.send_raw_iq !== "function") return [];
  const nodeAttr = node ? ` node="${escapeXml(node)}"` : "";
  try {
    const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#items"${nodeAttr}/></iq>`);
    return Array.from(responseXml.matchAll(/<item\b([^>]*)\/?>(?:<\/item>)?/g)).map(([, attrs]) => ({
      jid: readXmlAttr(attrs, "jid"),
      node: readXmlAttr(attrs, "node"),
      name: readXmlAttr(attrs, "name"),
    }));
  } catch {
    return [];
  }
}

async function rawDiscoInfo(xmpp: XmppSendIq, jid: string): Promise<{ features: string[] }> {
  if (typeof xmpp.getDiscoInfo === "function") {
    const info = await xmpp.getDiscoInfo(jid);
    return { features: info.features ?? [] };
  }
  if (typeof xmpp.send_raw_iq !== "function") return { features: [] };
  const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>`);
  const features = Array.from(responseXml.matchAll(/<feature\b[^>]*var=["']([^"']+)["'][^>]*\/?>(?:<\/feature>)?/g))
    .map(([, value]) => decodeXml(value))
    .filter(Boolean);
  return { features };
}

export async function invokeExtensionLaunch(
  xmpp: XmppSendIq,
  userJid: string,
  launch: ExtensionLaunchDescriptor,
): Promise<ExtensionCommandResult> {
  const sendRawIq = requireRawIq(xmpp);
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const iq = buildExtensionLaunchInvokeIq(userJid, launch, serviceJid);
  const fields = iq.command.form?.fields ?? [];
  const responseXml = buildCommandIqXml(serviceJid, iq.command.node, iq.command.action, fields);
  return parseCommandIqResponse(await sendRawIq(responseXml));
}

export async function invokeExtensionCommand(
  xmpp: XmppSendIq,
  userJid: string,
  command: DiscoveredExtensionCommand,
): Promise<ExtensionCommandResult> {
  const sendRawIq = requireRawIq(xmpp);
  const serviceJid = command.serviceJid || await discoverExtensionCommandService(xmpp, userJid);
  const responseXml = await sendRawIq(buildCommandIqXml(serviceJid, command.node, "execute"));
  return parseCommandIqResponse(responseXml);
}

export async function submitExtensionCommandForm(
  xmpp: XmppSendIq,
  command: DiscoveredExtensionCommand,
  sessionId: string,
  fields: ExtensionCommandFormField[],
  action: ExtensionCommandAction = "complete",
  _roomJid?: string,
): Promise<ExtensionCommandResult> {
  const sendRawIq = requireRawIq(xmpp);
  const dataFields: DataFormField[] = (action === "cancel" || action === "prev") ? [] : fields
    .filter((field) => field.type !== "fixed")
    .map((field) => ({ name: field.name, value: dataFormFieldValue(field) }));
  const responseXml = await sendRawIq(buildCommandIqXml(command.serviceJid, command.node, action, dataFields, sessionId));
  return parseCommandIqResponse(responseXml);
}

export async function discoverExtensionCommands(
  xmpp: XmppSendIq,
  userJid: string,
): Promise<DiscoveredExtensionCommand[]> {
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const items = await rawDiscoItems(xmpp, serviceJid, NS_ADHOC_COMMANDS);
  const filtered = items.filter((item): item is { jid?: string; node: string; name?: string } => !!item.node && item.node !== INVOKE_COMMAND_NODE);
  const commands: DiscoveredExtensionCommand[] = [];
  for (const item of filtered) {
    const itemServiceJid = item.jid ?? serviceJid;
    const node = item.node;
    let scope: ExtensionCommandScope = "global";
    try {
      const info = await rawDiscoInfoFull(xmpp, itemServiceJid, node);
      const parsedScope = parseExtensionCommandScope(info.extensions);
      if (parsedScope) scope = parsedScope;
    } catch {
      // If disco#info is unavailable for the command, fall back to global
      // scope; the worst case is showing a command that turns out to be
      // channel-only when invoked.
    }
    commands.push({
      serviceJid: itemServiceJid,
      node,
      name: item.name || node,
      scope,
    });
  }
  return commands;
}

function parseExtensionCommandScope(extensions: unknown[] | undefined): ExtensionCommandScope | null {
  if (!Array.isArray(extensions)) return null;
  for (const form of extensions) {
    const fields = (form as { fields?: unknown[] } | undefined)?.fields;
    if (!Array.isArray(fields)) continue;
    const formType = formFieldValue(fields, "FORM_TYPE");
    if (formType !== NS_WADDLE_EXTENSION_1 && formType !== EXTENSION_COMMAND_FORM_TYPE) continue;
    const value = formFieldValue(fields, "waddle#command_scope");
    if (value === "global" || value === "channel") return value;
  }
  return null;
}

function parseFieldOptions(options: unknown): ExtensionCommandFormOption[] {
  if (!Array.isArray(options)) return [];
  return options.flatMap((option) => {
    const item = option as { label?: unknown; value?: unknown; values?: unknown[] };
    const rawValue = item.value ?? item.values?.[0];
    if (typeof rawValue !== "string" && typeof rawValue !== "number" && typeof rawValue !== "boolean") return [];
    const value = String(rawValue);
    return [{
      label: typeof item.label === "string" && item.label ? item.label : value,
      value,
    }];
  });
}

function dataFormFieldValue(field: ExtensionCommandFormField): string | string[] | boolean {
  if (field.type === "boolean") return field.value === "1" || field.value === "true";
  if (field.type === "hidden" && field.values.length > 1) return field.values;
  if (field.type === "list-multi" || field.type === "text-multi" || field.type === "jid-multi") {
    return field.values.length > 0 ? field.values : [];
  }
  return field.value;
}

function parseCommandActions(actions: unknown, status?: string, actionsProvided = false): ExtensionCommandActions | undefined {
  const value = (actions && typeof actions === "object" ? actions : {}) as {
    execute?: unknown;
    next?: unknown;
    prev?: unknown;
    previous?: unknown;
    complete?: unknown;
    cancel?: unknown;
    allowed?: unknown[];
  };
  const allowed = new Set<ExtensionCommandAction>();
  if (actions && typeof actions === "object") {
    if (Array.isArray(value.allowed)) {
      for (const action of value.allowed) {
        if (isExtensionCommandAction(action)) allowed.add(action);
      }
    }
    if (value.next !== undefined) allowed.add("next");
    if (value.prev !== undefined || value.previous !== undefined) allowed.add("prev");
    if (value.complete !== undefined) allowed.add("complete");
    if (value.cancel !== undefined) allowed.add("cancel");
  }
  const execute = isExtensionCommandAction(value.execute) ? value.execute : undefined;
  if (execute) allowed.add(execute);
  if (status === "executing" && !actionsProvided) allowed.add("complete");
  if (status === "executing") allowed.add("cancel");
  const allowedList = [...allowed];
  return allowedList.length > 0 || execute ? { ...(execute ? { execute } : {}), allowed: allowedList } : undefined;
}

function isExtensionCommandAction(value: unknown): value is ExtensionCommandAction {
  return value === "next" || value === "prev" || value === "complete" || value === "cancel";
}

function isForbiddenExtensionCommandField(name: string, type: string): boolean {
  if (type === "text-private") return true;
  return /(?:^|[#:_-])(secret|token|password|api[_-]?key|apikey|credential)(?:$|[#:_-])/i.test(name);
}

async function discoverExtensionCommandService(xmpp: XmppSendIq, userJid: string): Promise<string> {
  const domain = jidDomain(userJid);
  const fallbackServiceJid = `extensions.${domain}`;

  try {
    const items = await rawDiscoItems(xmpp, domain);
    const candidates = items.map((item) => item.jid).filter((jid): jid is string => !!jid);
    for (const candidate of [domain, fallbackServiceJid, ...candidates.filter((jid) => jid !== fallbackServiceJid && jid !== domain)]) {
      try {
        const info = await rawDiscoInfo(xmpp, candidate);
        if (info.features.some((feature) => feature === NS_ADHOC_COMMANDS)) return candidate;
      } catch {
        // Try the next discovered component.
      }
    }
  } catch {
    // Fall through to returning the conventional extension service JID.
  }
  return fallbackServiceJid;
}

function parseDiscoInfoExtensions(xml: string): Array<{ fields: Array<{ var: string; value?: string; values?: string[] }> }> {
  const forms: Array<{ fields: Array<{ var: string; value?: string; values?: string[] }> }> = [];
  for (const [, formContent] of xml.matchAll(/<x\b[^>]*xmlns=["']jabber:x:data["'][^>]*>([\s\S]*?)<\/x>/g)) {
    const fields: Array<{ var: string; value?: string; values?: string[] }> = [];
    for (const [, attrs, fieldContent] of formContent.matchAll(/<field\b([^>]*)>([\s\S]*?)<\/field>/g)) {
      const varName = readXmlAttr(attrs, "var");
      if (!varName) continue;
      const values = Array.from(fieldContent.matchAll(/<value[^>]*>([\s\S]*?)<\/value>/g))
        .map(([, v]) => decodeXml(v.trim()));
      fields.push({ var: varName, value: values[0], values });
    }
    forms.push({ fields });
  }
  return forms;
}

async function rawDiscoInfoFull(xmpp: XmppSendIq, jid: string, node?: string): Promise<{ features: string[]; extensions: unknown[] }> {
  if (typeof xmpp.getDiscoInfo === "function") {
    try {
      const info = await xmpp.getDiscoInfo(jid, node);
      return { features: info.features ?? [], extensions: info.extensions ?? [] };
    } catch {
      return { features: [], extensions: [] };
    }
  }
  if (typeof xmpp.send_raw_iq !== "function") return { features: [], extensions: [] };
  const nodeAttr = node ? ` node="${escapeXml(node)}"` : "";
  try {
    const responseXml = await xmpp.send_raw_iq(
      `<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#info"${nodeAttr}/></iq>`,
    );
    const features = Array.from(responseXml.matchAll(/<feature\b[^>]*var=["']([^"']+)["'][^>]*\/?>(?:<\/feature>)?/g))
      .map(([, value]) => decodeXml(value))
      .filter(Boolean);
    return { features, extensions: parseDiscoInfoExtensions(responseXml) };
  } catch {
    return { features: [], extensions: [] };
  }
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


function formFieldValue(fields: unknown[], name: string): string | null {
  const field = fields
    .map((value) => value as FormFieldLike)
    .find((value) => (typeof value.name === "string" ? value.name : value.var) === name);
  const values = formFieldValues(field);
  return values[0] ?? null;
}

function formFieldValues(field: FormFieldLike | undefined): string[] {
  if (!field) return [];
  const values = Array.isArray(field.value)
    ? field.value
    : Array.isArray(field.values)
      ? field.values
      : Array.isArray(field.rawValues)
        ? field.rawValues
        : field.value !== undefined
          ? [field.value]
          : [];
  return values
    .filter((value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
    .map((value) => String(value));
}
