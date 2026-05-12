import type { ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import { jidDomain } from "../jid";
import { NS_WADDLE_EXTENSION_1 } from "./constants";
import type { DataFormField, ExtensionInvokeIq } from "./types";

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
      node: requiredLaunchValue(launch.commandNode, "command node"),
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
