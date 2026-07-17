import type { PinPermission } from "@/lib/chat-types";
import { escapeXml } from "./extension-commands/xml";

const NS_DATAFORM = "jabber:x:data";
const NS_MUC_OWNER = "http://jabber.org/protocol/muc#owner";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_BOOKMARKS_1 = "urn:xmpp:bookmarks:1";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
const NS_MUC_ROOMCONFIG = "http://jabber.org/protocol/muc#roomconfig";
const NS_PUBSUB_NODE_CONFIG = "http://jabber.org/protocol/pubsub#node_config";

interface RawIqClient {
  send_raw_iq?: (xml: string) => Promise<string>;
}

interface RoomLifecycleClient {
  join_room?: (roomJid: string, nick: string) => Promise<void>;
  leave_room?: (roomJid: string, nick: string) => Promise<void>;
}

type MucRoomClient = RawIqClient & RoomLifecycleClient;

export const FIELD_PIN_PERMISSION = "urn:waddle:roomconfig:pinpermission";
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

function slugifyNodeId(value: string): string {
  return value.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "") || "space";
}

async function sendIq(client: RawIqClient, type: "get" | "set", xmlPayload: string, to: string): Promise<string> {
  if (!client.send_raw_iq) throw new Error("XMPP session is not ready");
  return client.send_raw_iq(`<iq type="${type}" id="${crypto.randomUUID()}" to="${to}">${xmlPayload}</iq>`);
}

async function joinRoom(client: RoomLifecycleClient, roomJid: string, nick: string): Promise<void> {
  // #1255: `join_room` itself always requests `<history maxstanzas='0'/>`
  // (XEP-0045 §7.2.15) — MAM catch-up is the authoritative history source.
  if (!client.join_room) throw new Error("XMPP session is not ready");
  await client.join_room(roomJid, nick);
}

async function leaveRoom(client: RoomLifecycleClient, roomJid: string, nick: string): Promise<void> {
  if (!client.leave_room) throw new Error("XMPP session is not ready");
  return client.leave_room(roomJid, nick);
}

async function fetchMucOwnerConfig(
  client: RawIqClient,
  roomJid: string,
): Promise<Map<string, string>> {
  const responseXml = await sendIq(
    client,
    "get",
    `<query xmlns="${NS_MUC_OWNER}"/>`,
    roomJid,
  );
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
  client: RawIqClient,
  roomJid: string,
  params: ConfigureMucRoomParams,
): Promise<void> {
  const current = await fetchMucOwnerConfig(client, roomJid);
  current.set(FIELD_ROOM_NAME, params.name);
  if (params.description !== undefined) current.set(FIELD_ROOM_DESC, params.description);
  if (params.pinPermission) current.set(FIELD_PIN_PERMISSION, params.pinPermission);
  current.set("FORM_TYPE", NS_MUC_ROOMCONFIG);
  const fieldsXml = Array.from(current.entries())
    .map(([name, value]) => `<field var="${escapeXml(name)}"><value>${escapeXml(value)}</value></field>`)
    .join("");
  await sendIq(
    client,
    "set",
    `<query xmlns="${NS_MUC_OWNER}"><x xmlns="${NS_DATAFORM}" type="submit">${fieldsXml}</x></query>`,
    roomJid,
  );
}

export async function createMucRoom(client: MucRoomClient, mucServiceJid: string, params: CreateMucRoomParams): Promise<{ roomJid: string }> {
  const roomJid = `${params.roomLocalpart}@${mucServiceJid}`;
  await joinRoom(client, roomJid, params.nick);
  const fields = [
    dataField("FORM_TYPE", NS_MUC_ROOMCONFIG),
    dataField("muc#roomconfig_roomname", params.name),
    ...(params.description ? [dataField("muc#roomconfig_roomdesc", params.description)] : []),
    ...(params.mucType === "forum" ? [dataField("muc#roomconfig_forum", "1")] : []),
  ];
  await sendIq(
    client,
    "set",
    `<query xmlns="${NS_MUC_OWNER}"><x xmlns="${NS_DATAFORM}" type="submit">${fields.map((field) => `<field var="${escapeXml(field.name)}"><value>${escapeXml(field.value)}</value></field>`).join("")}</x></query>`,
    roomJid,
  );
  try { await leaveRoom(client, roomJid, params.nick); } catch {}
  return { roomJid };
}

export async function createSpaceNode(client: RawIqClient, spacesServiceJid: string, params: CreateSpaceNodeParams): Promise<{ node: string; serviceJid: string }> {
  const nodeId = params.nodeId ?? slugifyNodeId(params.name);
  const fields = [
    dataField("FORM_TYPE", NS_PUBSUB_NODE_CONFIG),
    dataField("pubsub#type", NS_SPACES_0),
    dataField("pubsub#title", params.name),
    dataField("pubsub#access_model", "open"),
    ...(params.description ? [dataField("pubsub#description", params.description)] : []),
  ];
  await sendIq(
    client,
    "set",
    `<pubsub xmlns="${NS_PUBSUB}"><create node="${escapeXml(nodeId)}"/><configure node="${escapeXml(nodeId)}"><x xmlns="${NS_DATAFORM}" type="submit">${fields.map((field) => `<field var="${escapeXml(field.name)}"><value>${escapeXml(field.value)}</value></field>`).join("")}</x></configure></pubsub>`,
    spacesServiceJid,
  );
  return { node: nodeId, serviceJid: spacesServiceJid };
}

async function publishMucToSpace(client: RawIqClient, spacesServiceJid: string, spaceNode: string, mucJid: string, params: PublishMucToSpaceParams): Promise<void> {
  await sendIq(
    client,
    "set",
    `<pubsub xmlns="${NS_PUBSUB}"><publish node="${escapeXml(spaceNode)}"><item id="${escapeXml(mucJid)}"><conference xmlns="${NS_BOOKMARKS_1}" name="${escapeXml(params.name)}" autojoin="${params.autojoin === false ? "false" : "true"}"/></item></publish></pubsub>`,
    spacesServiceJid,
  );
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
  client: RawIqClient,
  spacesServiceJid: string,
  targetSpaceNode: string,
  mucJid: string,
  params: PublishMucToSpaceParams,
): Promise<void> {
  await publishMucToSpace(client, spacesServiceJid, targetSpaceNode, mucJid, params);
}

export async function createMucInSpace(client: MucRoomClient, mucServiceJid: string, spacesServiceJid: string, params: CreateMucInSpaceParams): Promise<{ roomJid: string; spaceNode: string; spacesServiceJid: string }> {
  const { roomJid } = await createMucRoom(client, mucServiceJid, params);
  await publishMucToSpace(client, spacesServiceJid, params.spaceNode, roomJid, { name: params.name, autojoin: true });
  return { roomJid, spaceNode: params.spaceNode, spacesServiceJid };
}

export async function createSpaceWithMuc(client: MucRoomClient, mucServiceJid: string, spacesServiceJid: string, params: CreateSpaceWithMucParams): Promise<{ roomJid: string; spaceNode: string; spacesServiceJid: string }> {
  const { node: spaceNode } = await createSpaceNode(client, spacesServiceJid, { nodeId: params.spaceNodeId, name: params.spaceName, description: params.spaceDescription });
  const { roomJid } = await createMucRoom(client, mucServiceJid, { roomLocalpart: params.roomLocalpart, nick: params.nick, name: params.mucName, description: params.mucDescription, mucType: params.mucType });
  await publishMucToSpace(client, spacesServiceJid, spaceNode, roomJid, { name: params.mucName, autojoin: true });
  return { roomJid, spaceNode, spacesServiceJid };
}
