import type { WaddleClient } from "@waddle/xmpp-client-wasm";

const NS_DATAFORM = "jabber:x:data";
const NS_MUC_OWNER = "http://jabber.org/protocol/muc#owner";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
export const NS_BOOKMARKS_1 = "urn:xmpp:bookmarks:1";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
const NS_MUC_ROOMCONFIG = "http://jabber.org/protocol/muc#roomconfig";
const NS_PUBSUB_NODE_CONFIG = "http://jabber.org/protocol/pubsub#node_config";

type HybridClient = Partial<WaddleClient> & {
  sendIQ?: (iq: Record<string, unknown>) => Promise<unknown>;
  joinRoom?: (roomJid: string, nick: string, opts?: unknown) => Promise<void>;
  leaveRoom?: (roomJid: string, nick: string) => Promise<void>;
};

interface CreateMucRoomParams {
  roomLocalpart: string;
  nick: string;
  name: string;
  description?: string;
  mucType?: "text" | "forum";
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

function escapeXml(value: string): string {
  return value.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&apos;");
}

function dataField(name: string, value: string) {
  return { name, value };
}

function slugifyNodeId(value: string): string {
  return value.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "") || "space";
}

async function sendIq(client: HybridClient, iq: Record<string, unknown>, xmlPayload: string, to: string): Promise<unknown> {
  if (client.sendIQ) return client.sendIQ(iq);
  return client.send_raw_iq?.(`<iq type="${iq.type}" id="${crypto.randomUUID()}" to="${to}">${xmlPayload}</iq>`);
}

async function joinRoom(client: HybridClient, roomJid: string, nick: string): Promise<void> {
  if (client.joinRoom) return client.joinRoom(roomJid, nick, { history: { maxStanzas: 0 } });
  return client.join_room?.(roomJid, nick);
}

async function leaveRoom(client: HybridClient, roomJid: string, nick: string): Promise<void> {
  if (client.leaveRoom) return client.leaveRoom(roomJid, nick);
  return client.leave_room?.(roomJid, nick);
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
  await sendIq(
    client,
    { type: "set", to: roomJid, muc: { type: "configure", form: { type: "submit", fields } } },
    `<query xmlns="${NS_MUC_OWNER}"><x xmlns="${NS_DATAFORM}" type="submit">${fields.map((field) => `<field var="${escapeXml(field.name)}"><value>${escapeXml(field.value)}</value></field>`).join("")}</x></query>`,
    roomJid,
  );
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
  await sendIq(
    client,
    { type: "set", to: spacesServiceJid, pubsub: { create: { node: nodeId }, configure: { node: nodeId, form: { type: "submit", fields } } } },
    `<pubsub xmlns="${NS_PUBSUB}"><create node="${escapeXml(nodeId)}"/><configure node="${escapeXml(nodeId)}"><x xmlns="${NS_DATAFORM}" type="submit">${fields.map((field) => `<field var="${escapeXml(field.name)}"><value>${escapeXml(field.value)}</value></field>`).join("")}</x></configure></pubsub>`,
    spacesServiceJid,
  );
  return { node: nodeId, serviceJid: spacesServiceJid };
}

export async function publishMucToSpace(client: HybridClient, spacesServiceJid: string, spaceNode: string, mucJid: string, params: PublishMucToSpaceParams): Promise<void> {
  await sendIq(
    client,
    { type: "set", to: spacesServiceJid, pubsub: { publish: { node: spaceNode, item: { id: mucJid, content: { itemType: NS_BOOKMARKS_1, name: params.name, autojoin: params.autojoin !== false } } } } },
    `<pubsub xmlns="${NS_PUBSUB}"><publish node="${escapeXml(spaceNode)}"><item id="${escapeXml(mucJid)}"><conference xmlns="${NS_BOOKMARKS_1}" name="${escapeXml(params.name)}" autojoin="${params.autojoin === false ? "false" : "true"}"/></item></publish></pubsub>`,
    spacesServiceJid,
  );
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
