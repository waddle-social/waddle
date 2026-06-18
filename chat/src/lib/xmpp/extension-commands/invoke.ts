import type { ExtensionLaunchDescriptor } from "@/lib/chat-ui";
import { discoverExtensionCommandService } from "./disco";
import { dataFormFieldValue } from "./form-fields";
import { buildExtensionLaunchInvokeIq } from "./launch";
import { requireRawIq } from "./transport";
import type {
  DataFormField,
  DiscoveredExtensionCommand,
  ExtensionCommandAction,
  ExtensionCommandFormField,
  ExtensionCommandResult,
  XmppSendIq,
} from "./types";
import { buildCommandIqXml, parseCommandIqResponse } from "./xml";

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
  roomJid?: string,
): Promise<ExtensionCommandResult> {
  const sendRawIq = requireRawIq(xmpp);
  const serviceJid = command.serviceJid || await discoverExtensionCommandService(xmpp, userJid);
  const fields: DataFormField[] = roomJid ? [{ name: "waddle#room_jid", value: roomJid }] : [];
  const responseXml = await sendRawIq(buildCommandIqXml(serviceJid, command.node, "execute", fields));
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
  const sendRawIq = requireRawIq(xmpp);
  const dataFields: DataFormField[] = (action === "cancel" || action === "prev") ? [] : [
    ...fields
      .filter((field) => field.type !== "fixed")
      .map((field) => ({ name: field.name, value: dataFormFieldValue(field) })),
    ...(roomJid && !fields.some((field) => field.name === "waddle#room_jid")
      ? [{ name: "waddle#room_jid", value: roomJid }]
      : []),
  ];
  const responseXml = await sendRawIq(buildCommandIqXml(command.serviceJid, command.node, action, dataFields, sessionId));
  return parseCommandIqResponse(responseXml);
}
