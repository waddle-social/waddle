import type { WaddleClient } from "@waddle/xmpp-client-wasm";
import type { PinPermission } from "@/lib/chat-types";
import { buildIqXml, type XmlElementSpec } from "./xml-build";

const NS_DATAFORM = "jabber:x:data";
const NS_MUC_OWNER = "http://jabber.org/protocol/muc#owner";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_BOOKMARKS_1 = "urn:xmpp:bookmarks:1";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
const NS_MUC_ROOMCONFIG = "http://jabber.org/protocol/muc#roomconfig";
const NS_PUBSUB_NODE_CONFIG = "http://jabber.org/protocol/pubsub#node_config";

type HybridClient = Partial<WaddleClient> & {
  send_raw_iq?: (xml: string) => Promise<string>;
};

export const FIELD_PIN_PERMISSION = "urn:waddle:roomconfig:pinpermission";
/** Whether accounts on other servers may reach the room (owner form, off by default). */
export const FIELD_ROOM_FEDERATED = "urn:waddle:roomconfig:federated";
const FIELD_ROOM_NAME = "muc#roomconfig_roomname";
const FIELD_ROOM_DESC = "muc#roomconfig_roomdesc";

interface CreateMucRoomParams {
  roomLocalpart: string;
  nick: string;
  name: string;
  description?: string;
  mucType?: "text" | "forum";
}

interface ConfigureMucRoomParams {
  name: string;
  description?: string;
  /** #415: per-room pin permission. */
  pinPermission?: PinPermission;
  /** Whether accounts on other servers may reach the room; written only when the form offers the field. */
  federated?: boolean;
}
interface CreateSpaceNodeParams { nodeId?: string; name: string; description?: string; }
interface PublishMucToSpaceParams { name: string; autojoin?: boolean; }
interface CreateMucInSpaceParams extends CreateMucRoomParams { spaceNode: string; }
interface CreateSpaceWithMucParams {
  spaceNodeId?: string;
  spaceName: string;
  spaceDescription?: string;
  roomLocalpart: string;
  nick: string;
  mucName: string;
  mucDescription?: string;
  mucType?: "text" | "forum";
}

function dataField(name: string, value: string) {
  return { name, value };
}

/** XEP-0004 `submit` form carrying each field with one value. */
function submitForm(fields: ReadonlyArray<{ name: string; value: string }>): XmlElementSpec {
  return {
    name: "x",
    ns: NS_DATAFORM,
    attrs: { type: "submit" },
    children: fields.map((field) => ({
      name: "field",
      attrs: { var: field.name },
      children: [{ name: "value", text: field.value }],
    })),
  };
}

function slugifyNodeId(value: string): string {
  return value.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "") || "space";
}

/** Build the IQ with the DOM, so `to` and every payload value are escaped. */
async function sendIq(client: HybridClient, type: "get" | "set", payload: XmlElementSpec, to: string): Promise<string> {
  if (!client.send_raw_iq) throw new Error("XMPP session is not ready");
  return client.send_raw_iq(buildIqXml({ type, id: crypto.randomUUID(), to, payload }));
}

async function joinRoom(client: HybridClient, roomJid: string, nick: string): Promise<void> {
  // #1255: `join_room` itself always requests `<history maxstanzas='0'/>`
  // (XEP-0045 §7.2.15) — MAM catch-up is the authoritative history source.
  if (!client.join_room) throw new Error("XMPP session is not ready");
  await client.join_room(roomJid, nick);
}

async function leaveRoom(client: HybridClient, roomJid: string, nick: string): Promise<void> {
  if (!client.leave_room) throw new Error("XMPP session is not ready");
  return client.leave_room(roomJid, nick);
}

async function fetchMucOwnerConfig(
  client: HybridClient,
  roomJid: string,
): Promise<Map<string, string>> {
  const responseXml = await sendIq(client, "get", { name: "query", ns: NS_MUC_OWNER }, roomJid);
  const doc = new DOMParser().parseFromString(responseXml, "text/xml");
  const form = doc.getElementsByTagNameNS(NS_DATAFORM, "x")[0];
  const fields = new Map<string, string>();
  if (!form) return fields;
  for (const field of Array.from(form.children)) {
    if (field.localName !== "field" || field.namespaceURI !== NS_DATAFORM) continue;
    const name = field.getAttribute("var");
    if (!name) continue;
    // Read the field's own <value> direct child, not <option><value>.
    // list-single fields contain both a current <value> and several
    // <option> elements that each carry their own <value>.
    const valueChild = Array.from(field.children).find(
      (child) => child.localName === "value" && child.namespaceURI === NS_DATAFORM,
    );
    fields.set(name, valueChild?.textContent ?? "");
  }
  return fields;
}

/** #415: XEP-0045 §10.2 owner-config GET-then-SET. Owner-only.
 *
 * Fetches the current room config form, applies the caller's
 * overrides on top, and submits the merged form. This is the
 * conformant pattern: fields the caller does NOT supply round-trip
 * through unchanged so the server doesn't revert un-echoed fields
 * to defaults. */
export async function configureMucRoom(
  client: HybridClient,
  roomJid: string,
  params: ConfigureMucRoomParams,
): Promise<void> {
  const current = await fetchMucOwnerConfig(client, roomJid);
  current.set(FIELD_ROOM_NAME, params.name);
  if (params.description !== undefined) current.set(FIELD_ROOM_DESC, params.description);
  if (params.pinPermission) current.set(FIELD_PIN_PERMISSION, params.pinPermission);
  if (params.federated !== undefined && current.has(FIELD_ROOM_FEDERATED)) {
    current.set(FIELD_ROOM_FEDERATED, params.federated ? "1" : "0");
  }
  current.set("FORM_TYPE", NS_MUC_ROOMCONFIG);
  const fields = Array.from(current.entries()).map(([name, value]) => dataField(name, value));
  await sendIq(client, "set", { name: "query", ns: NS_MUC_OWNER, children: [submitForm(fields)] }, roomJid);
}

export async function createMucRoom(client: HybridClient, mucServiceJid: string, params: CreateMucRoomParams): Promise<{ roomJid: string }> {
  const roomJid = `${params.roomLocalpart}@${mucServiceJid}`;
  await joinRoom(client, roomJid, params.nick);
  const fields = [
    dataField("FORM_TYPE", NS_MUC_ROOMCONFIG),
    dataField("muc#roomconfig_roomname", params.name),
    ...(params.description ? [dataField("muc#roomconfig_roomdesc", params.description)] : []),
    ...(params.mucType === "forum" ? [dataField("muc#roomconfig_forum", "1")] : []),
  ];
  await sendIq(client, "set", { name: "query", ns: NS_MUC_OWNER, children: [submitForm(fields)] }, roomJid);
  try { await leaveRoom(client, roomJid, params.nick); } catch {}
  return { roomJid };
}

export async function createSpaceNode(client: HybridClient, spacesServiceJid: string, params: CreateSpaceNodeParams): Promise<{ node: string; serviceJid: string }> {
  const nodeId = params.nodeId ?? slugifyNodeId(params.name);
  const fields = [
    dataField("FORM_TYPE", NS_PUBSUB_NODE_CONFIG),
    dataField("pubsub#type", NS_SPACES_0),
    dataField("pubsub#title", params.name),
    dataField("pubsub#access_model", "open"),
    ...(params.description ? [dataField("pubsub#description", params.description)] : []),
  ];
  await sendIq(client, "set", {
    name: "pubsub",
    ns: NS_PUBSUB,
    children: [
      { name: "create", attrs: { node: nodeId } },
      { name: "configure", attrs: { node: nodeId }, children: [submitForm(fields)] },
    ],
  }, spacesServiceJid);
  return { node: nodeId, serviceJid: spacesServiceJid };
}

async function publishMucToSpace(client: HybridClient, spacesServiceJid: string, spaceNode: string, mucJid: string, params: PublishMucToSpaceParams): Promise<void> {
  await sendIq(client, "set", {
    name: "pubsub",
    ns: NS_PUBSUB,
    children: [{
      name: "publish",
      attrs: { node: spaceNode },
      children: [{
        name: "item",
        attrs: { id: mucJid },
        children: [{
          name: "conference",
          ns: NS_BOOKMARKS_1,
          attrs: { name: params.name, autojoin: params.autojoin === false ? "false" : "true" },
        }],
      }],
    }],
  }, spacesServiceJid);
}

/**
 * Move a MUC bookmark to a different XEP-0503 Space.
 *
 * Implemented as a publish to the target Space — the server-side
 * `handle_spaces_publish` enforces XEP-0503 single-membership by
 * retracting the same `item-id` from every other Space first, so
 * the client doesn't have to issue a separate retract.
 */
export async function moveMucToSpace(
  client: HybridClient,
  spacesServiceJid: string,
  targetSpaceNode: string,
  mucJid: string,
  params: PublishMucToSpaceParams,
): Promise<void> {
  await publishMucToSpace(client, spacesServiceJid, targetSpaceNode, mucJid, params);
}

export async function createMucInSpace(client: HybridClient, mucServiceJid: string, spacesServiceJid: string, params: CreateMucInSpaceParams): Promise<{ roomJid: string; spaceNode: string; spacesServiceJid: string }> {
  const { roomJid } = await createMucRoom(client, mucServiceJid, params);
  await publishMucToSpace(client, spacesServiceJid, params.spaceNode, roomJid, { name: params.name, autojoin: true });
  return { roomJid, spaceNode: params.spaceNode, spacesServiceJid };
}

export async function createSpaceWithMuc(client: HybridClient, mucServiceJid: string, spacesServiceJid: string, params: CreateSpaceWithMucParams): Promise<{ roomJid: string; spaceNode: string; spacesServiceJid: string }> {
  const { node: spaceNode } = await createSpaceNode(client, spacesServiceJid, { nodeId: params.spaceNodeId, name: params.spaceName, description: params.spaceDescription });
  const { roomJid } = await createMucRoom(client, mucServiceJid, { roomLocalpart: params.roomLocalpart, nick: params.nick, name: params.mucName, description: params.mucDescription, mucType: params.mucType });
  await publishMucToSpace(client, spacesServiceJid, spaceNode, roomJid, { name: params.mucName, autojoin: true });
  return { roomJid, spaceNode, spacesServiceJid };
}
