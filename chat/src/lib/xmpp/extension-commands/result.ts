import type { ExtensionAnnotationAction, ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import { parseCommandActions, parseExtensionCommandForm } from "./form-fields";
import type {
  ExtensionCommandNote,
  ExtensionCommandOutcome,
  ExtensionCommandResult,
} from "./types";

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
