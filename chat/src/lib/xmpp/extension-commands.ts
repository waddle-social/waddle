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
  notes: ExtensionCommandNote[];
  form?: unknown;
}

export interface ExtensionCommandFormField {
  name: string;
  label: string;
  type: string;
  value: string;
  values: string[];
  required: boolean;
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
  value: string;
}

interface ExtensionInvokeIq {
  type: "set";
  to: string;
  command: {
    node: string;
    action: "execute";
    form: {
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
    { name: "source-stanza-id", value: requiredLaunchValue(messageStanzaId, "source stanza id") },
    { name: "launch-id", value: requiredLaunchValue(launch.id, "launch id") },
    { name: "launch-token", value: requiredLaunchValue(launch.launchToken, "launch token") },
    { name: "expires-at", value: requiredLaunchValue(launch.expiresAt, "expiry") },
    { name: "waddle#waddle_id", value: requiredLaunchValue(launch.context.waddleId, "waddle id") },
    { name: "waddle#message_stanza_id", value: requiredLaunchValue(messageStanzaId, "source stanza id") },
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
      form: {
        type: "submit",
        fields: [
          { name: "FORM_TYPE", type: "hidden", value: NS_ADHOC_COMMANDS },
        ],
      },
    },
  };
}

export function parseExtensionCommandResult(response: unknown): ExtensionCommandResult {
  const command = (response as { command?: { status?: unknown; sessionid?: unknown; sessionId?: unknown; notes?: Array<{ type?: unknown; value?: unknown }>; form?: unknown } } | undefined)?.command;
  const notes = Array.isArray(command?.notes) ? command.notes : [];
  return {
    ...(typeof command?.status === "string" && command.status ? { status: command.status } : {}),
    ...(typeof command?.sessionid === "string" && command.sessionid ? { sessionId: command.sessionid } : {}),
    ...(typeof command?.sessionId === "string" && command.sessionId ? { sessionId: command.sessionId } : {}),
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
    const item = field as { name?: unknown; var?: unknown; label?: unknown; type?: unknown; value?: unknown; values?: unknown[]; required?: unknown };
    const name = typeof item.name === "string" ? item.name : typeof item.var === "string" ? item.var : "";
    if (!name) return [];
    const values = Array.isArray(item.values) ? item.values : item.value !== undefined ? [item.value] : [];
    const stringValues = values
      .filter((value) => typeof value === "string" || typeof value === "number" || typeof value === "boolean")
      .map((value) => String(value));
    return [{
      name,
      label: typeof item.label === "string" && item.label ? item.label : name,
      type: typeof item.type === "string" ? item.type : "text-single",
      value: stringValues[0] ?? "",
      values: stringValues,
      required: item.required === true,
    }];
  });
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
): Promise<ExtensionCommandResult> {
  const response = await xmpp.sendIQ({
    type: "set",
    to: command.serviceJid,
    command: {
      node: command.node,
      sessionid: sessionId,
      action: "complete",
        form: {
          type: "submit",
          fields: fields.map((field) => ({
            name: field.name,
            type: field.type,
            value: field.value,
            values: field.values.length > 0 ? field.values : [field.value],
          })),
        },
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
