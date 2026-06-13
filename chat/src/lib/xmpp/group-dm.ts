import { jidDomain } from "./jid";
import type { XmppSendIq } from "./extension-commands/types";
import { escapeXml, parseCommandIqResponse } from "./extension-commands/xml";

export const GROUP_DM_CREATE_NODE = "urn:waddle:group-dm:create:0";
const NS_COMMANDS = "http://jabber.org/protocol/commands";
const NS_DATA = "jabber:x:data";

export interface CreateGroupDmInput {
  userJid: string;
  name: string;
  memberJids: string[];
}

export interface CreateGroupDmResult {
  roomJid: string;
}

function fieldXml(name: string, values: string[], type?: string): string {
  const typeAttr = type ? ` type="${escapeXml(type)}"` : "";
  return `<field var="${escapeXml(name)}"${typeAttr}>${values.map((value) => `<value>${escapeXml(value)}</value>`).join("")}</field>`;
}

export function buildCreateGroupDmCommandXml(input: CreateGroupDmInput): string {
  const fields = [
    fieldXml("FORM_TYPE", [GROUP_DM_CREATE_NODE], "hidden"),
    fieldXml("name", [input.name]),
    fieldXml("member_jids", input.memberJids, "jid-multi"),
  ].join("");
  return `<iq type="set" id="${crypto.randomUUID()}" to="${escapeXml(jidDomain(input.userJid))}"><command xmlns="${NS_COMMANDS}" node="${GROUP_DM_CREATE_NODE}" action="execute"><x xmlns="${NS_DATA}" type="submit">${fields}</x></command></iq>`;
}

export function parseCreateGroupDmResult(xml: string): CreateGroupDmResult {
  const result = parseCommandIqResponse(xml);
  const form = result.form as { fields?: Array<{ name?: string; var?: string; value?: string; values?: string[] }> } | undefined;
  const roomJid = form?.fields
    ?.find((field) => (field.name || field.var) === "room_jid")
    ?.values?.[0]
    ?? form?.fields?.find((field) => (field.name || field.var) === "room_jid")?.value
    ?? "";
  if (!roomJid) {
    throw new Error("Group-DM create response did not include room_jid.");
  }
  return { roomJid };
}

export async function createGroupDm(
  xmpp: XmppSendIq,
  input: CreateGroupDmInput,
): Promise<CreateGroupDmResult> {
  if (!xmpp.send_raw_iq) {
    throw new Error("XMPP raw IQ transport is unavailable.");
  }
  const response = await xmpp.send_raw_iq(buildCreateGroupDmCommandXml(input));
  return parseCreateGroupDmResult(response);
}
