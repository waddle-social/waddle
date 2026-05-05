import type { WaddleClient } from "@waddle/xmpp-client-wasm";
import { normalizeChannelType } from "@/lib/channel-types";
import type { DiscoveredChannel, DiscoveredSpace, DiscoveredTopology } from "./types";
import { barePeerJid, jidDomain } from "./jid";

const NS_FORUMS_0 = "urn:xmpp:forums:0";
const NS_MUC = "http://jabber.org/protocol/muc";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_METADATA = `${NS_PUBSUB}#meta-data`;
const DISCO_INFO_NS = "http://jabber.org/protocol/disco#info";
const DISCO_ITEMS_NS = "http://jabber.org/protocol/disco#items";
const DATAFORM_NS = "jabber:x:data";
const FIELD_FORUM_MODE = "muc#roomconfig_forum";

type DiscoveryRole = "owner" | "admin" | "moderator" | "member" | null;

type HybridClient = Partial<WaddleClient> & {
  send_raw_iq?: (xml: string) => Promise<string>;
};

type DiscoInfoData = {
  features: string[];
  identities: Array<{ category?: string; type?: string; name?: string }>;
  fields: Map<string, string>;
};

function parseXml(xml: string): Document {
  return new DOMParser().parseFromString(xml, "text/xml");
}

function elementChildren(element: Element, localName: string, namespace: string): Element[] {
  return Array.from(element.children).filter((child) => child.localName === localName && child.namespaceURI === namespace);
}

function textContent(element: Element | null | undefined): string | null {
  const value = element?.textContent?.trim();
  return value ? value : null;
}

function parseSpaceNodeIri(value: string | null): string | null {
  if (!value) return null;
  try {
    const iri = new URL(value);
    const node = iri.searchParams.get(";node") ?? iri.searchParams.get("node");
    return node?.trim() || null;
  } catch {
    const match = /(?:^|[?&;])node=([^&;]+)/.exec(value);
    return match?.[1] ? decodeURIComponent(match[1]).trim() || null : null;
  }
}

function parseBooleanValue(value: string | null): boolean | null {
  if (!value) return null;
  const normalized = value.toLowerCase();
  if (["1", "true", "yes"].includes(normalized)) return true;
  if (["0", "false", "no"].includes(normalized)) return false;
  return null;
}

function parseDiscoveryRole(value: string | null): DiscoveryRole {
  switch (value) {
    case "owner": return "owner";
    case "admin":
    case "publisher": return "admin";
    case "moderator": return "moderator";
    case "member": return "member";
    default: return null;
  }
}

function channelTypeFromInfo(info: DiscoInfoData): DiscoveredChannel["channelType"] {
  const forumField = parseBooleanValue(info.fields.get(FIELD_FORUM_MODE) ?? null);
  return normalizeChannelType(info.features.includes(NS_FORUMS_0) || forumField ? "forum" : "text");
}

function roomParentSpaceId(info: DiscoInfoData): string | null {
  return parseSpaceNodeIri(info.fields.get("parent") ?? null)
    ?? parseSpaceNodeIri(info.fields.get("muc#roominfo_pubsub") ?? null);
}

async function sendDiscoInfo(xmpp: HybridClient, to: string, node?: string): Promise<DiscoInfoData | null> {
  if (!xmpp.send_raw_iq) return null;
  const id = crypto.randomUUID();
  const nodeAttr = node ? ` node="${node}"` : "";
  const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${id}" to="${to}"><query xmlns="${DISCO_INFO_NS}"${nodeAttr}/></iq>`);
  const query = parseXml(responseXml).getElementsByTagNameNS(DISCO_INFO_NS, "query")[0];
  if (!query) return null;
  const fields = new Map<string, string>();
  for (const form of elementChildren(query, "x", DATAFORM_NS)) {
    for (const field of elementChildren(form, "field", DATAFORM_NS)) {
      const name = field.getAttribute("var");
      if (!name) continue;
      const value = textContent(field.querySelector("value"));
      if (value) fields.set(name, value);
    }
  }
  return {
    features: elementChildren(query, "feature", DISCO_INFO_NS).map((feature) => feature.getAttribute("var") ?? "").filter(Boolean),
    identities: elementChildren(query, "identity", DISCO_INFO_NS).map((identity) => ({ category: identity.getAttribute("category") ?? undefined, type: identity.getAttribute("type") ?? undefined, name: identity.getAttribute("name") ?? undefined })),
    fields,
  };
}

async function sendDiscoItems(xmpp: HybridClient, to: string, node?: string): Promise<Array<{ jid?: string; name?: string; node?: string }>> {
  if (!xmpp.send_raw_iq) return [];
  const id = crypto.randomUUID();
  const nodeAttr = node ? ` node="${node}"` : "";
  const xml = `<iq type="get" id="${id}" to="${to}"><query xmlns="${DISCO_ITEMS_NS}"${nodeAttr}/></iq>`;
  let responseXml: string;
  try {
    responseXml = await xmpp.send_raw_iq(xml);
  } catch (err) {
    console.error("[disco] send_raw_iq items FAILED", { to, err });
    throw err;
  }
  const doc = parseXml(responseXml);
  const query = doc.getElementsByTagNameNS(DISCO_ITEMS_NS, "query")[0];
  if (!query) return [];
  return Array.from(query.getElementsByTagNameNS(DISCO_ITEMS_NS, "item"))
    .filter((item) => item.parentNode === query)
    .map((item) => ({ jid: item.getAttribute("jid") ?? undefined, name: item.getAttribute("name") ?? undefined, node: item.getAttribute("node") ?? undefined }));
}

async function sendPubsubItems(xmpp: HybridClient, to: string, node: string): Promise<Array<{ jid?: string; name?: string }>> {
  if (!xmpp.send_raw_iq) return [];
  const id = crypto.randomUUID();
  const responseXml = await xmpp.send_raw_iq(`<iq type="get" id="${id}" to="${to}"><pubsub xmlns="${NS_PUBSUB}"><items node="${node}"/></pubsub></iq>`);
  const doc = parseXml(responseXml);
  const items = doc.getElementsByTagNameNS(NS_PUBSUB, "items")[0];
  if (!items) return [];
  return Array.from(items.getElementsByTagNameNS(NS_PUBSUB, "item"))
    .filter((item) => item.parentNode === items)
    .map((item) => {
      const conference = item.getElementsByTagName("conference")[0];
      return {
        jid: item.getAttribute("id") ?? conference?.getAttribute("jid") ?? undefined,
        name: conference?.getAttribute("name") ?? undefined,
      };
    });
}

export function spacesServiceDomain(jid: string): string { return `spaces.${jidDomain(jid)}`; }
export function mucServiceDomain(jid: string): string { return `muc.${jidDomain(jid)}`; }

async function discoverComponentServices(xmpp: HybridClient, domain: string, jid: string): Promise<{ muc: string; spaces: string }> {
  const fallback = { muc: mucServiceDomain(jid), spaces: spacesServiceDomain(jid) };
  try {
    const items = await sendDiscoItems(xmpp, domain);
    const candidates = items.map((item) => item.jid).filter((value): value is string => !!value);
    let muc = fallback.muc;
    let spaces = fallback.spaces;
    await Promise.all(candidates.map(async (serviceJid) => {
      try {
        const info = await sendDiscoInfo(xmpp, serviceJid);
        if (!info) return;
        if (info.features.includes(NS_MUC) || info.identities.some((identity) => identity.category === "conference")) muc = serviceJid;
        if (info.features.includes(NS_SPACES_0) || info.identities.some((identity) => identity.category === "pubsub")) spaces = serviceJid;
      } catch {}
    }));
    return { muc, spaces };
  } catch {
    return fallback;
  }
}

function channelFromRoom(room: { jid: string; name?: string; channel_type?: string }, position: number): DiscoveredChannel {
  const id = barePeerJid(room.jid).split("@")[0] ?? room.jid;
  return { id, name: room.name || id, jid: room.jid, channelType: normalizeChannelType(room.channel_type || "text"), position };
}

async function hydrateRoomInfo(xmpp: HybridClient, room: DiscoveredChannel): Promise<DiscoveredChannel> {
  if (!room.jid) return room;
  try {
    const info = await sendDiscoInfo(xmpp, room.jid);
    if (!info) return room;
    const parentSpaceId = roomParentSpaceId(info);
    return {
      ...room,
      channelType: channelTypeFromInfo(info),
      ...(parentSpaceId ? { spaceId: parentSpaceId, standalone: false } : {}),
      ...(info.features.length ? { features: info.features } : {}),
    };
  } catch {
    return room;
  }
}

export async function discoverChannels(xmpp: HybridClient, jid: string): Promise<DiscoveredChannel[]> {
  const spaces = await sendDiscoItems(xmpp, spacesServiceDomain(jid));
  const spaceNode = spaces[0]?.node;
  if (!spaceNode) return [];
  const items = await sendPubsubItems(xmpp, spacesServiceDomain(jid), spaceNode);
  const hydrated = await Promise.all(items.map((item, position) => hydrateRoomInfo(xmpp, channelFromRoom({ jid: item.jid ?? "", name: item.name }, position))));
  return hydrated.map((room, position) => ({ id: room.id, name: room.name, channelType: room.channelType, position }));
}

export async function discoverTopology(xmpp: HybridClient, jid: string): Promise<DiscoveredTopology> {
  const domain = jidDomain(jid);
  const services = await discoverComponentServices(xmpp, domain, jid);
  let rooms: DiscoveredChannel[] = [];
  const bookmarkedSpaceIds = new Map<string, string>();
  const spaces: DiscoveredSpace[] = [];
  let serverRole: DiscoveryRole = null;

  try {
    const serverInfo = await sendDiscoInfo(xmpp, domain);
    if (serverInfo) serverRole = parseDiscoveryRole(serverInfo.fields.get("waddle#server_affiliation") ?? null);
  } catch {}

  try {
    const spaceItems = await sendDiscoItems(xmpp, services.spaces);
    for (const [index, item] of spaceItems.entries()) {
      const spaceId = item.node ?? barePeerJid(item.jid ?? "").split("@")[0] ?? `space-${index}`;
      let role = serverRole;
      if (item.node) {
        try {
          const info = await sendDiscoInfo(xmpp, services.spaces, item.node);
          if (info) role = parseDiscoveryRole(info.fields.get("pubsub#affiliation") ?? null) ?? serverRole;
        } catch {}
        try {
          const bookmarks = await sendPubsubItems(xmpp, services.spaces, item.node);
          for (const bookmark of bookmarks) {
            if (bookmark.jid) bookmarkedSpaceIds.set(barePeerJid(bookmark.jid), spaceId);
          }
        } catch {}
      }
      spaces.push({ id: spaceId, name: item.name ?? spaceId, role });
    }
  } catch {}

  const mucRooms = await sendDiscoItems(xmpp, services.muc);
  const hydrated = await Promise.all(mucRooms.map((room, position) => hydrateRoomInfo(xmpp, channelFromRoom({ jid: room.jid ?? "", name: room.name }, position))));
  rooms = hydrated.map((room, position) => ({ ...room, position, ...(room.jid && bookmarkedSpaceIds.get(barePeerJid(room.jid)) ? { spaceId: bookmarkedSpaceIds.get(barePeerJid(room.jid)), standalone: false } : {}) }));

  const roomSpaceIds = new Map(rooms.flatMap((room) => room.jid ? [[barePeerJid(room.jid), room.spaceId]] as const : []));
  return {
    spaces,
    rooms: rooms.map((room, position) => ({ ...room, position, ...(room.jid && roomSpaceIds.get(barePeerJid(room.jid)) ? { standalone: false } : { standalone: room.standalone ?? true }) })),
    serverRole,
    services,
  };
}
