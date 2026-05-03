import type { ExtensionAnnotationAction, ExtensionLaunchDescriptor } from "@/lib/chat-ui";import { jidDomain } from "./jid";

interface XmppSendIq {
  send_raw_iq(xml: string): Promise<string>;
}

function escapeXml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&apos;");
}
const NS_WADDLE_EXTENSION_1 = "urn:waddle:extension:1";
const NS_ADHOC_COMMANDS = "http://jabber.org/protocol/commands";
const INVOKE_COMMAND_NODE = "urn:waddle:extension:1:invoke";
const EXTENSION_ROUTE_FORM_TYPE = "urn:waddle:extension:1:routes";
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

interface DiscoItem {
  jid?: string;
  node?: string;
  name?: string;
}

interface DiscoInfo {
  features?: string[];
  extensions?: unknown[];
}

interface DiscoClient {
  getDiscoItems?: (jid: string, node?: string) => Promise<{ items?: DiscoItem[] }>;
  getDiscoInfo?: (jid: string, node?: string) => Promise<DiscoInfo>;
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

function buildExtensionCommandExecuteIq(serviceJid: string, node: string): ExtensionInvokeIq {
  return {
    type: "set",
    to: serviceJid,
    command: {
      node,
      action: "execute",
    },
  };
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

function parseCommandIqResponse(xml: string): ExtensionCommandResult {
  const doc = new DOMParser().parseFromString(xml, "text/xml");
  const command = doc.querySelector(`command[xmlns="${NS_ADHOC_COMMANDS}"]`);
  if (!command) return { notes: [] };
  const status = command.getAttribute("status") ?? undefined;
  const sessionId = command.getAttribute("sessionid") ?? command.getAttribute("sid") ?? undefined;
  const notes: ExtensionCommandNote[] = Array.from(command.querySelectorAll("note"))
    .map((note) => ({ type: note.getAttribute("type") ?? undefined, value: note.textContent ?? "" }))
    .filter((note) => note.value.length > 0);
  const actionsEl = command.querySelector("actions");
  const actions = actionsEl ? {
    execute: actionsEl.getAttribute("execute") ?? undefined,
    next: !!actionsEl.querySelector("next") || undefined,
    prev: !!actionsEl.querySelector("prev") || !!actionsEl.querySelector("previous") || undefined,
    complete: !!actionsEl.querySelector("complete") || undefined,
    cancel: !!actionsEl.querySelector("cancel") || undefined,
  } : undefined;
  const formEl = command.querySelector(`x[xmlns="jabber:x:data"]`);
  const form = formEl ? { type: formEl.getAttribute("type") ?? "form", fields: Array.from(formEl.querySelectorAll("field")).map((field) => ({
    name: field.getAttribute("var") ?? "",
    var: field.getAttribute("var") ?? "",
    type: field.getAttribute("type") ?? "text-single",
    label: field.getAttribute("label") ?? undefined,
    desc: field.querySelector("desc")?.textContent ?? undefined,
    value: field.querySelector("value")?.textContent ?? "",
    values: Array.from(field.querySelectorAll("value")).map((v) => v.textContent ?? ""),
    required: !!field.querySelector("required"),
    options: Array.from(field.querySelectorAll("option")).map((opt) => ({
      label: opt.getAttribute("label") ?? undefined,
      value: opt.querySelector("value")?.textContent ?? "",
    })),
  })) } : undefined;
  return {
    ...(status ? { status } : {}),
    ...(sessionId ? { sessionId } : {}),
    notes,
    ...(actions ? { actions: parseCommandActions(actions, status, !!actionsEl) } : {}),
    ...(form ? { form } : {}),
  };
}

async function rawDiscoItems(xmpp: XmppSendIq, jid: string, node?: string): Promise<Array<{ jid?: string; node?: string; name?: string }>> {
  const nodeAttr = node ? ` node="${escapeXml(node)}"` : "";
  try {
    const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#items"${nodeAttr}/></iq>`);
    const doc = new DOMParser().parseFromString(responseXml, "text/xml");
    return Array.from(doc.querySelectorAll(`query[xmlns="http://jabber.org/protocol/disco#items"] > item`)).map((item) => ({
      jid: item.getAttribute("jid") ?? undefined,
      node: item.getAttribute("node") ?? undefined,
      name: item.getAttribute("name") ?? undefined,
    }));
  } catch {
    return [];
  }
}

async function rawDiscoInfo(xmpp: XmppSendIq, jid: string): Promise<{ features: string[] }> {
  const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${crypto.randomUUID()}" to="${escapeXml(jid)}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>`);
  const doc = new DOMParser().parseFromString(responseXml, "text/xml");
  const features = Array.from(doc.querySelectorAll(`query[xmlns="http://jabber.org/protocol/disco#info"] > feature`))
    .map((f) => f.getAttribute("var") ?? "")
    .filter(Boolean);
  return { features };
}

export async function invokeExtensionLaunch(
  xmpp: XmppSendIq,
  userJid: string,
  launch: ExtensionLaunchDescriptor,
): Promise<ExtensionCommandResult> {
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const iq = buildExtensionLaunchInvokeIq(userJid, launch, serviceJid);
  const fields = iq.command.form?.fields ?? [];
  const responseXml = await buildCommandIqXml(serviceJid, iq.command.node, iq.command.action, fields);
  return parseCommandIqResponse(await xmpp.send_raw_iq(responseXml));
}

export async function invokeExtensionCommand(
  xmpp: XmppSendIq,
  userJid: string,
  command: DiscoveredExtensionCommand,
): Promise<ExtensionCommandResult> {
  const serviceJid = command.serviceJid || await discoverExtensionCommandService(xmpp, userJid);
  const responseXml = await xmpp.send_raw_iq(buildCommandIqXml(serviceJid, command.node, "execute"));
  return parseCommandIqResponse(responseXml);
}

export async function submitExtensionCommandForm(
  xmpp: XmppSendIq,
  command: DiscoveredExtensionCommand,
  sessionId: string,
  fields: ExtensionCommandFormField[],
  action: ExtensionCommandAction = "complete",
  roomJid?: string,
): Promise<ExtensionCommandResult> {
  const dataFields: DataFormField[] = (action === "cancel" || action === "prev") ? [] : fields
    .filter((field) => field.type !== "fixed")
    .map((field) => ({ name: field.name, value: dataFormFieldValue(field) }));
  const responseXml = await xmpp.send_raw_iq(buildCommandIqXml(command.serviceJid, command.node, action, dataFields, sessionId));
  return parseCommandIqResponse(responseXml);}

export async function discoverExtensionCommands(
  xmpp: XmppSendIq,
  userJid: string,
): Promise<DiscoveredExtensionCommand[]> {
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const disco = xmpp as unknown as DiscoClient;
  const response = await disco.getDiscoItems?.(serviceJid, NS_ADHOC_COMMANDS);
  const items = response?.items ?? [];
  const filtered = items.filter((item) => item.node && item.node !== INVOKE_COMMAND_NODE);
  const commands: DiscoveredExtensionCommand[] = [];
  for (const item of filtered) {
    const itemServiceJid = item.jid ?? serviceJid;
    const node = item.node!;
    let scope: ExtensionCommandScope = "global";
    try {
      const info = await disco.getDiscoInfo?.(itemServiceJid, node);
      const parsedScope = parseExtensionCommandScope(info?.extensions);
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
    if (formFieldValue(fields, "FORM_TYPE") !== EXTENSION_COMMAND_FORM_TYPE) continue;
    const value = formFieldValue(fields, "waddle#command_scope");
    if (value === "global" || value === "channel") return value;
  }
  return null;
}

export async function discoverExtensionRoutes(
  xmpp: Agent,
  userJid: string,
): Promise<DiscoveredExtensionRoute[]> {
  const serviceJid = await discoverExtensionServiceJid(xmpp, userJid);
  const disco = xmpp as unknown as DiscoClient;
  const response = await disco.getDiscoItems?.(serviceJid);
  const routes: DiscoveredExtensionRoute[] = [];
  for (const item of response?.items ?? []) {
    if (!item.node) continue;
    const itemServiceJid = item.jid ?? serviceJid;
    const info = await disco.getDiscoInfo?.(itemServiceJid, item.node);
    routes.push(...(info?.extensions ?? []).flatMap((form) => parseExtensionRouteForm(form, itemServiceJid) ?? []));
  }
  if (routes.length > 0) return routes;

  const legacyInfo = await disco.getDiscoInfo?.(serviceJid);
  return (legacyInfo?.extensions ?? []).flatMap((form) => parseExtensionRouteForm(form, serviceJid) ?? []);
}

export function resolveExtensionRouteStateNode(route: DiscoveredExtensionRoute, roomJid: string): string {
  return route.stateNode.replaceAll("{room}", roomJid);
}

export async function fetchExtensionRouteItems(
  xmpp: Agent,
  route: DiscoveredExtensionRoute,
  roomJid: string,
): Promise<ExtensionRouteItem[]> {
  const node = resolveExtensionRouteStateNode(route, roomJid);
  let response: { items?: Array<{ id?: string; content?: unknown }> } | undefined;
  try {
    response = await (xmpp as unknown as {
      getItems?: (jid: string, node: string, opts?: { max?: number }) => Promise<{ items?: Array<{ id?: string; content?: unknown }> }>;
    }).getItems?.(route.serviceJid, node, { max: 100 });
  } catch (error) {
    throw normalizeXmppRouteError(error);
  }
  return (response?.items ?? []).flatMap((item) => {
    const view = parseExtensionItemView(item.content, item.id);
    return view ? [view] : [];
  });
}

function normalizeXmppRouteError(error: unknown): Error {
  if (error instanceof Error) return error;
  const condition = xmppErrorCondition(error) ?? xmppPubsubErrorCondition(error);
  if (condition === "forbidden") return new Error("Extension route access was denied.");
  if (condition === "item-not-found") return new Error("Extension route was not found.");
  if (condition === "service-unavailable") return new Error("Extension route service is unavailable.");
  if (condition === "timeout" || condition === "remote-server-timeout") {
    return new Error("Extension route request timed out.");
  }
  if (condition) return new Error(`Extension route load failed: ${condition}.`);
  return new Error("Could not load extension route.");
}

function xmppErrorCondition(error: unknown): string | null {
  if (!error || typeof error !== "object") return null;
  const candidate = error as Record<string, unknown>;
  if (typeof candidate.condition === "string") return candidate.condition;
  const nested = candidate.error;
  if (nested && typeof nested === "object") {
    const nestedCondition = (nested as Record<string, unknown>).condition;
    if (typeof nestedCondition === "string") return nestedCondition;
  }
  return null;
}

function xmppPubsubErrorCondition(error: unknown): string | null {
  if (!error || typeof error !== "object") return null;
  const candidate = error as Record<string, unknown>;
  if (typeof candidate.pubsubError === "string") return candidate.pubsubError;
  const nested = candidate.error;
  if (nested && typeof nested === "object") {
    const nestedCondition = (nested as Record<string, unknown>).pubsubError;
    if (typeof nestedCondition === "string") return nestedCondition;
  }
  return null;
}

function parseExtensionItemView(content: unknown, id: string | undefined): ExtensionRouteItem | null {
  const root = normalizePayloadElement(content, NS_WADDLE_EXTENSION_1);
  if (!root) return null;
  if (root.namespace !== NS_WADDLE_EXTENSION_1) return null;
  if (root.name !== "extension-item") return null;

  const view: ExtensionRouteItem = {
    fields: [],
    options: [],
    actions: [],
  };
  if (id) view.id = id;

  for (const child of root.children) {
    if (child.namespace !== NS_WADDLE_EXTENSION_1) continue;
    switch (child.name) {
      case "title": {
        const text = (child.text ?? "").trim();
        if (text) view.title = text;
        break;
      }
      case "subtitle": {
        const text = (child.text ?? "").trim();
        if (text) view.subtitle = text;
        break;
      }
      case "description": {
        const text = (child.text ?? "").trim();
        if (text) view.description = text;
        break;
      }
      case "link": {
        const href = child.attributes.href?.trim();
        if (href) view.link = { href };
        break;
      }
      case "field": {
        const name = child.attributes.name?.trim();
        if (!name) break;
        const value = (child.text ?? "").trim();
        const label = child.attributes.label?.trim();
        view.fields.push(label ? { name, label, value } : { name, value });
        break;
      }
      case "option": {
        const optionId = child.attributes.id?.trim();
        const label = child.attributes.label?.trim();
        if (optionId && label) view.options.push({ id: optionId, label });
        break;
      }
      case "action": {
        const launchId = child.attributes["launch-id"]?.trim();
        const label = child.attributes.label?.trim();
        if (launchId && label) view.actions.push({ launchId, label });
        break;
      }
      default:
        break;
    }
  }

  return view;
} 2750256 (fix(chat): resolve TypeScript build errors in WASM client migration)

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

async function discoverExtensionCommandService(xmpp: XmppSendIq, userJid: string): Promise<string> {  const domain = jidDomain(userJid);
  const disco = xmpp as unknown as DiscoClient;
  const candidates: string[] = [];

  try {
    const items = await rawDiscoItems(xmpp, domain);
    const candidates = items.map((item) => item.jid).filter((jid): jid is string => !!jid);
    for (const candidate of [domain, fallback, ...candidates.filter((jid) => jid !== fallback && jid !== domain)]) {
      try {
        const info = await rawDiscoInfo(xmpp, candidate);
        if (info.features.some((feature) => feature === NS_ADHOC_COMMANDS)) return candidate;
      } catch {
        // Try the next discovered component.
      }    }
  } catch {
    // Fall through to direct disco#info on the conventional component JID.
  }
  if (!candidates.includes(fallbackServiceJid)) candidates.push(fallbackServiceJid);

  for (const candidate of candidates) {
    try {
      const info = await disco.getDiscoInfo?.(candidate);
      if (info?.features?.some((feature) => feature === NS_WADDLE_EXTENSION_1)) return candidate;
    } catch {
      // Keep probing other discovered items before using the conventional fallback.
    }
  }
  return fallbackServiceJid;
}

async function discoverExtensionCommandService(xmpp: Agent, userJid: string): Promise<string> {
  const serviceJid = await discoverExtensionServiceJid(xmpp, userJid);
  try {
    const info = await (xmpp as unknown as DiscoClient).getDiscoInfo?.(serviceJid);
    if (info?.features?.some((feature) => feature === NS_ADHOC_COMMANDS)) return serviceJid;
  } catch {
    // Fall back to the discovered service JID when disco#info is unavailable.
  }
  return serviceJid;
}

function parseExtensionRouteForm(form: unknown, serviceJid: string): DiscoveredExtensionRoute | null {
  const fields = (form as { fields?: unknown[] } | undefined)?.fields;
  if (!Array.isArray(fields)) return null;
  if (formFieldValue(fields, "FORM_TYPE") !== EXTENSION_ROUTE_FORM_TYPE) return null;
  const pluginId = formFieldValue(fields, "waddle#plugin_id");
  const routeId = formFieldValue(fields, "waddle#route_id");
  const label = formFieldValue(fields, "waddle#route_label");
  const scope = formFieldValue(fields, "waddle#route_scope");
  const surface = formFieldValue(fields, "waddle#route_surface");
  const stateNode = formFieldValue(fields, "waddle#state_node");
  const payloadNamespace = formFieldValue(fields, "waddle#payload_ns");
  if (
    !pluginId
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

function normalizePayloadElement(content: unknown, fallbackNamespace: string): ExtensionPayloadElement | null {
  if (!content || typeof content !== "object") return null;
  if (isRawXmlElement(content)) return normalizeRawXmlPayload(content, fallbackNamespace);
  const item = content as {
    name?: unknown;
    elementName?: unknown;
    itemType?: unknown;
    namespace?: unknown;
    xmlns?: unknown;
    attributes?: Record<string, unknown>;
    children?: unknown[];
    text?: unknown;
    value?: unknown;
  };
  const name = typeof item.name === "string"
    ? item.name
    : typeof item.elementName === "string"
      ? item.elementName
      : "";
  if (!name) return null;
  const rawAttributes = item.attributes ?? primitiveObjectFields(content);
  const attributes = Object.fromEntries(
    Object.entries(rawAttributes)
      .filter(([, value]) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
      .map(([key, value]) => [key, String(value)]),
  );
  const namespace = firstNonEmpty([
    attributes.xmlns,
    typeof item.xmlns === "string" ? item.xmlns : "",
    typeof item.namespace === "string" ? item.namespace : "",
    typeof item.itemType === "string" && item.itemType.startsWith("urn:") ? item.itemType : "",
    fallbackNamespace,
  ]);
  const childValues = Array.isArray(item.children) ? item.children : [];
  const text = [
    typeof item.text === "string" ? item.text : "",
    typeof item.value === "string" ? item.value : "",
    ...childValues.filter((child): child is string => typeof child === "string"),
  ].join("").trim();
  return {
    namespace,
    name,
    attributes: { xmlns: namespace, ...attributes },
    ...(text ? { text } : {}),
    children: childValues.flatMap((child) => typeof child === "string" ? [] : normalizePayloadElement(child, namespace) ?? []),
  };
}

type RawXmlElement = {
  getNamespace: () => string;
  getName: () => string;
  attributes: Record<string, unknown>;
  children: unknown[];
};

function isRawXmlElement(value: unknown): value is RawXmlElement {
  return !!value
    && typeof value === "object"
    && typeof (value as RawXmlElement).getNamespace === "function"
    && typeof (value as RawXmlElement).getName === "function"
    && Array.isArray((value as RawXmlElement).children);
}

function normalizeRawXmlPayload(xml: RawXmlElement, fallbackNamespace: string): ExtensionPayloadElement {
  const namespace = firstNonEmpty([xml.getNamespace(), fallbackNamespace]);
  const attributes = Object.fromEntries(
    Object.entries(xml.attributes)
      .filter(([, value]) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
      .map(([key, value]) => [key, String(value)]),
  );
  const childValues = xml.children;
  const text = childValues.filter((child): child is string => typeof child === "string").join("").trim();
  return {
    namespace,
    name: xml.getName(),
    attributes: { xmlns: namespace, ...attributes },
    ...(text ? { text } : {}),
    children: childValues.flatMap((child) =>
      isRawXmlElement(child) ? [normalizeRawXmlPayload(child, namespace)] : []
    ),
  };
}

function firstNonEmpty(values: string[]): string {
  return values.find((value) => value.trim().length > 0) ?? "";
}

function primitiveObjectFields(value: unknown): Record<string, string> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const skip = new Set(["attributes", "children", "payload", "content", "name", "elementName", "namespace", "xmlns", "itemType"]);
  return Object.fromEntries(
    Object.entries(value as Record<string, unknown>)
      .filter(([key, field]) =>
        !skip.has(key)
        && (typeof field === "string" || typeof field === "number" || typeof field === "boolean")
      )
      .map(([key, field]) => [key, String(field)]),
  );
}
