// The owner's `federated` setting of a room and of a Space.
//
// A room or Space of this deployment is reached by accounts on other
// servers only while its owner has turned this setting on. It is off by
// default and is one switch for every server this deployment federates
// with. Each lives in the owner's configuration form:
//
// - rooms: `{urn:waddle:roomconfig}federated` in the XEP-0045 `muc#owner`
//   form (written through `configureMucRoom`);
// - Spaces: `{urn:waddle:spaces}federated` in the XEP-0060 `pubsub#owner`
//   configure form of the Spaces service.
//
// A server that does not offer the field leaves it out of the form; the
// read then answers `null` and the setting is not shown.

import { FIELD_ROOM_FEDERATED } from "./protocol-helpers";
import {
  buildIqXml,
  childElements,
  firstElementNS,
  parseXmlResponse,
  sendRawIq,
  submitField,
  type RawIqClient,
} from "./xml-stanza";

export const FIELD_SPACE_FEDERATED = "{urn:waddle:spaces}federated";

const NS_DATAFORM = "jabber:x:data";
const NS_MUC_OWNER = "http://jabber.org/protocol/muc#owner";
const NS_PUBSUB_OWNER = "http://jabber.org/protocol/pubsub#owner";
const NODE_CONFIG_FORM = "http://jabber.org/protocol/pubsub#node_config";
const FIELD_MAX_ITEMS = "pubsub#max_items";

/** A XEP-0004 boolean value, or `null` for anything else. */
export function parseFormBoolean(value: string | null | undefined): boolean | null {
  switch (value?.trim()) {
    case "1":
    case "true":
      return true;
    case "0":
    case "false":
      return false;
    default:
      return null;
  }
}

/**
 * The value of a boolean field in the first data form under `doc`, or `null`
 * when the form does not carry the field.
 */
export function formBooleanField(doc: Document | Element, fieldName: string): boolean | null {
  const form = firstElementNS(doc, NS_DATAFORM, "x");
  if (!form) return null;
  const field = childElements(form, "field", NS_DATAFORM).find((candidate) => candidate.getAttribute("var") === fieldName);
  if (!field) return null;
  // An offered boolean field without a value is false (XEP-0004 §3.3).
  const value = childElements(field, "value", NS_DATAFORM)[0]?.textContent ?? "0";
  return parseFormBoolean(value);
}

/** Read a room's `federated` setting from its owner form. Owner only. */
export async function fetchRoomFederated(xmpp: RawIqClient, roomJid: string): Promise<boolean | null> {
  const id = crypto.randomUUID();
  const xml = buildIqXml({ type: "get", id, to: roomJid, payload: { name: "query", ns: NS_MUC_OWNER } });
  return formBooleanField(parseXmlResponse(await sendRawIq(xmpp, { xml, id, to: roomJid })), FIELD_ROOM_FEDERATED);
}

function buildSpaceConfigureRequest(id: string, spacesServiceJid: string, node: string): string {
  return buildIqXml({
    type: "get",
    id,
    to: spacesServiceJid,
    payload: { name: "pubsub", ns: NS_PUBSUB_OWNER, children: [{ name: "configure", attrs: { node } }] },
  });
}

/**
 * The configure submission that changes the `federated` field. The Spaces
 * service applies a submitted form as a change to the node's
 * configuration, so most fields left out keep their values; `max_items` is
 * the exception, since the service resets an omitted `max_items` to
 * unlimited. The value read from the node's form is therefore sent back
 * unchanged when there is one.
 */
export function buildSpaceFederatedSubmission(
  id: string,
  spacesServiceJid: string,
  node: string,
  federated: boolean,
  currentMaxItems?: string | null,
): string {
  return buildIqXml({
    type: "set",
    id,
    to: spacesServiceJid,
    payload: {
      name: "pubsub",
      ns: NS_PUBSUB_OWNER,
      children: [{
        name: "configure",
        attrs: { node },
        children: [{
          name: "x",
          ns: NS_DATAFORM,
          attrs: { type: "submit" },
          children: [
            submitField("FORM_TYPE", NODE_CONFIG_FORM, "hidden"),
            ...(currentMaxItems ? [submitField(FIELD_MAX_ITEMS, currentMaxItems)] : []),
            submitField(FIELD_SPACE_FEDERATED, federated ? "1" : "0"),
          ],
        }],
      }],
    },
  });
}

async function fetchSpaceConfigureForm(xmpp: RawIqClient, spacesServiceJid: string, node: string): Promise<Document> {
  const id = crypto.randomUUID();
  const response = await sendRawIq(xmpp, { xml: buildSpaceConfigureRequest(id, spacesServiceJid, node), id, to: spacesServiceJid, node });
  return parseXmlResponse(response);
}

/** The value of a field in the first data form under `doc`, or `null`. */
function formFieldValue(doc: Document | Element, fieldName: string): string | null {
  const form = firstElementNS(doc, NS_DATAFORM, "x");
  if (!form) return null;
  const field = childElements(form, "field", NS_DATAFORM).find((candidate) => candidate.getAttribute("var") === fieldName);
  const value = field ? childElements(field, "value", NS_DATAFORM)[0]?.textContent?.trim() : undefined;
  return value || null;
}

/** Read a Space's `federated` setting from its owner configure form. */
export async function fetchSpaceFederated(xmpp: RawIqClient, spacesServiceJid: string, node: string): Promise<boolean | null> {
  return formBooleanField(await fetchSpaceConfigureForm(xmpp, spacesServiceJid, node), FIELD_SPACE_FEDERATED);
}

/**
 * Change a Space's `federated` setting, then read back the value its server
 * holds. The server may refuse the change, or fail after writing part of
 * it, so the owner is shown the value read back, with the error if the
 * change itself failed.
 */
export async function setSpaceFederated(
  xmpp: RawIqClient,
  spacesServiceJid: string,
  node: string,
  federated: boolean,
): Promise<{ current: boolean | null; error: unknown }> {
  const id = crypto.randomUUID();
  let error: unknown = null;
  try {
    const form = await fetchSpaceConfigureForm(xmpp, spacesServiceJid, node);
    await sendRawIq(xmpp, {
      xml: buildSpaceFederatedSubmission(id, spacesServiceJid, node, federated, formFieldValue(form, FIELD_MAX_ITEMS)),
      id,
      to: spacesServiceJid,
      node,
    });
  } catch (caught) {
    error = caught;
  }
  const current = await fetchSpaceFederated(xmpp, spacesServiceJid, node);
  return { current, error };
}
