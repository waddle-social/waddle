import type { Agent } from "stanza";
import type { ExtensionAnnotationAction, ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import { jidDomain } from "./jid";

const NS_WADDLE_EXTENSION_1 = "urn:waddle:extension:1";
const NS_ADHOC_COMMANDS = "http://jabber.org/protocol/commands";
const INVOKE_COMMAND_NODE = "urn:waddle:extension:1:invoke";

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

export interface DiscoveredExtensionCommand {
  serviceJid: string;
  node: string;
  name: string;
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

function optionalDataFormField(name: string, value: string | undefined): DataFormField[] {
  return value?.trim() ? [{ name, value }] : [];
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
    ...optionalDataFormField("source-stanza-id", messageStanzaId),
    { name: "launch-id", value: requiredLaunchValue(launch.id, "launch id") },
    { name: "launch-token", value: requiredLaunchValue(launch.launchToken, "launch token") },
    { name: "expires-at", value: requiredLaunchValue(launch.expiresAt, "expiry") },
    { name: "waddle#waddle_id", value: requiredLaunchValue(launch.context.waddleId, "waddle id") },
    ...optionalDataFormField("waddle#message_stanza_id", messageStanzaId),
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
  if (byName.get("FORM_TYPE") !== `${NS_WADDLE_EXTENSION_1}:result`) {
    return [];
  }
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

  const formDetail = extensionResultFormDetail(parsed.form);
  if (formDetail) return { state: "success", detail: formDetail };

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

function extensionResultFormDetail(form: unknown): string | undefined {
  const fields = parseExtensionCommandForm(form);
  const byName = new Map(fields.map((field) => [field.name, field.value.trim()]));
  if (byName.get("FORM_TYPE") !== `${NS_WADDLE_EXTENSION_1}:result`) return undefined;
  const body = byName.get("extension#body");
  const prompt = byName.get("extension#prompt");
  return [body, prompt]
    .filter((value): value is string => !!value)
    .join(" ")
    .trim() || undefined;
}

export async function invokeExtensionLaunch(
  xmpp: Agent,
  userJid: string,
  launch: ExtensionLaunchDescriptor,
): Promise<ExtensionCommandResult> {
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const response = await xmpp.sendIQ(
    buildExtensionLaunchInvokeIq(userJid, launch, serviceJid) as unknown as Parameters<Agent["sendIQ"]>[0],
  );
  return parseExtensionCommandResult(response);
}

export async function invokeExtensionCommand(
  xmpp: Agent,
  userJid: string,
  command: DiscoveredExtensionCommand,
): Promise<ExtensionCommandResult> {
  const serviceJid = command.serviceJid || await discoverExtensionCommandService(xmpp, userJid);
  const response = await xmpp.sendIQ(
    buildExtensionCommandExecuteIq(serviceJid, command.node) as unknown as Parameters<Agent["sendIQ"]>[0],
  );
  return parseExtensionCommandResult(response);
}

export async function submitExtensionCommandForm(
  xmpp: Agent,
  command: DiscoveredExtensionCommand,
  sessionId: string,
  fields: ExtensionCommandFormField[],
  action: ExtensionCommandAction = "complete",
): Promise<ExtensionCommandResult> {
  const response = await xmpp.sendIQ({
    type: "set",
    to: command.serviceJid,
    command: {
      node: command.node,
      sid: sessionId,
      action,
      ...(action === "cancel" || action === "prev" ? {} : {
          form: {
            type: "submit",
            fields: fields
              .filter((field) => field.type !== "fixed")
              .map((field) => ({
                name: field.name,
                type: field.type,
                value: dataFormFieldValue(field),
              })),
          },
        }),
    },
  } as unknown as Parameters<Agent["sendIQ"]>[0]);
  return parseExtensionCommandResult(response);
}

export async function discoverExtensionCommands(
  xmpp: Agent,
  userJid: string,
): Promise<DiscoveredExtensionCommand[]> {
  const serviceJid = await discoverExtensionCommandService(xmpp, userJid);
  const disco = xmpp as unknown as {
    getDiscoItems?: (jid: string, node?: string) => Promise<{ items?: Array<{ jid?: string; node?: string; name?: string }> }>;
  };
  const response = await disco.getDiscoItems?.(serviceJid, NS_ADHOC_COMMANDS);
  const items = response?.items ?? [];
  return items
    .filter((item) => item.node && item.node !== INVOKE_COMMAND_NODE)
    .map((item) => ({
      serviceJid: item.jid ?? serviceJid,
      node: item.node!,
      name: item.name || item.node!,
    }));
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

async function discoverExtensionCommandService(xmpp: Agent, userJid: string): Promise<string> {
  const domain = jidDomain(userJid);
  if (!domain) throw new Error("Cannot resolve extension service for this XMPP account.");
  const fallback = extensionServiceJidForUserJid(userJid);
  try {
    const response = await (xmpp as unknown as { getDiscoItems?: (jid: string) => Promise<{ items?: Array<{ jid?: string }> }> }).getDiscoItems?.(domain);
    const candidates = response?.items?.map((item) => item.jid).filter((jid): jid is string => !!jid) ?? [];
    for (const candidate of [domain, fallback, ...candidates.filter((jid) => jid !== fallback && jid !== domain)]) {
      try {
        const info = await (xmpp as unknown as { getDiscoInfo?: (jid: string) => Promise<{ features?: string[] }> }).getDiscoInfo?.(candidate);
        if (info?.features?.some((feature) => feature === NS_ADHOC_COMMANDS)) return candidate;
      } catch {
        // Try the next discovered component.
      }
    }
  } catch {
    // Fall back to the conventional component JID when discovery is unavailable.
  }
  return fallback;
}
