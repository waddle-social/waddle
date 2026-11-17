// Reading and following communities hosted on other servers.
//
// Every request here goes to the remote address itself, through the home
// server's federation router: XEP-0030 to learn what an address is,
// XEP-0060 to read a Space's content and to follow it. The answers are
// shown as the host gave them; nothing is inferred from a Space about its
// rooms (XEP-0503 defines no inheritance), and joining a room is a plain
// XEP-0045 join that the room's host decides.
//
// XEP-0503 0.3.0 lets a Space carry avatar and banner items whose metadata
// points at image URLs. This client never fetches them (ADR-0020 D4): the
// parsed content records only whether such an item exists, and keeps no URL.

import { canonicalDomain, canonicalRoomJid, parseSpaceNodeIri, type CommunityAddress } from "./remote-address";
import { stanzaErrorContext } from "./stanza-error-context";
import {
  buildIqXml,
  childElements,
  firstElementNS,
  parseXmlResponse,
  sendRawIq,
  type RawIqClient,
} from "./xml-stanza";

const NS_DISCO_INFO = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS = "http://jabber.org/protocol/disco#items";
const NS_DATAFORM = "jabber:x:data";
const NS_MUC = "http://jabber.org/protocol/muc";
const NS_MUC_ROOMINFO = "http://jabber.org/protocol/muc#roominfo";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_METADATA = "http://jabber.org/protocol/pubsub#meta-data";
const NS_SPACES = "urn:xmpp:spaces:0";
const NS_BOOKMARKS = "urn:xmpp:bookmarks:1";
const SPACE_AVATAR_ITEM = "urn:xmpp:spaces:avatar:metadata:0";
const SPACE_BANNER_ITEM = "urn:xmpp:spaces:banner:metadata:0";

/**
 * Budget for one request to a remote address. The home server keeps a
 * federated request open for 30 seconds and then answers
 * `remote-server-timeout` itself, which can take up to five seconds more
 * when the request's expiry is swept on another replica; waiting well past
 * that lets the definitive answer arrive instead of a local guess.
 */
const REMOTE_IQ_TIMEOUT_MS = 45_000;

/** Most entries a listing shows. */
const MAX_LISTED = 256;

export interface RemoteRoomFlags {
  membersOnly: boolean;
  passwordProtected: boolean;
  hidden: boolean;
  moderated: boolean;
  nonAnonymous: boolean;
}

export type RemoteDirectoryEntry =
  | { kind: "room"; roomJid: string; name: string | null }
  | { kind: "space"; serviceJid: string; node: string; name: string | null }
  | { kind: "service"; jid: string; name: string | null };

/** What a host says an address is. */
export type RemoteDescription =
  | {
    kind: "room";
    roomJid: string;
    name: string | null;
    description: string | null;
    flags: RemoteRoomFlags;
    /** The Space the room names as its parent, if any; a claim, not a grant. */
    parent: { serviceJid: string; node: string } | null;
  }
  | { kind: "space"; serviceJid: string; node: string; title: string | null; description: string | null }
  | { kind: "directory"; jid: string; name: string | null; entries: RemoteDirectoryEntry[] }
  | { kind: "other"; jid: string; name: string | null };

/** A room a Space lists. Listing it admits nothing; its host decides joins. */
interface RemoteSpaceRoom {
  roomJid: string;
  name: string | null;
  /** The Space's own suggestion; this client never joins on it alone. */
  suggestedAutojoin: boolean;
}

/**
 * A Space's content. Images are recorded only as present or absent: the
 * type has no field that could carry their URLs.
 */
export interface RemoteSpaceContent {
  rooms: RemoteSpaceRoom[];
  hasAvatar: boolean;
  hasBanner: boolean;
  /** Items that are neither rooms nor images, which this client ignores. */
  otherItems: number;
}

export type SpaceSubscriptionState = "subscribed" | "pending" | "unconfigured";

type DiscoInfo = {
  identities: Array<{ category: string; type: string; name: string | null }>;
  features: Set<string>;
  forms: Map<string, Map<string, string>>;
};

function discoInfoRequest(id: string, to: string, node?: string): string {
  return buildIqXml({
    type: "get",
    id,
    to,
    payload: { name: "query", ns: NS_DISCO_INFO, attrs: { node } },
  });
}

function discoItemsRequest(id: string, to: string, node?: string): string {
  return buildIqXml({
    type: "get",
    id,
    to,
    payload: { name: "query", ns: NS_DISCO_ITEMS, attrs: { node } },
  });
}

async function remoteIq(xmpp: RawIqClient, xml: string, id: string, to: string, node?: string): Promise<Document> {
  const response = await sendRawIq(xmpp, { xml, id, to, node, timeoutMs: REMOTE_IQ_TIMEOUT_MS });
  return parseXmlResponse(response);
}

/** Parse a `disco#info` result. */
export function parseDiscoInfo(doc: Document | Element): DiscoInfo {
  const query = firstElementNS(doc, NS_DISCO_INFO, "query");
  const info: DiscoInfo = { identities: [], features: new Set(), forms: new Map() };
  if (!query) return info;
  for (const identity of childElements(query, "identity", NS_DISCO_INFO)) {
    info.identities.push({
      category: identity.getAttribute("category") ?? "",
      type: identity.getAttribute("type") ?? "",
      name: identity.getAttribute("name")?.trim() || null,
    });
  }
  for (const feature of childElements(query, "feature", NS_DISCO_INFO)) {
    const name = feature.getAttribute("var");
    if (name) info.features.add(name);
  }
  for (const form of childElements(query, "x", NS_DATAFORM)) {
    const fields = new Map<string, string>();
    for (const field of childElements(form, "field", NS_DATAFORM)) {
      const name = field.getAttribute("var");
      const value = childElements(field, "value", NS_DATAFORM)[0]?.textContent?.trim();
      if (name && value) fields.set(name, value);
    }
    const formType = fields.get("FORM_TYPE");
    if (formType && !info.forms.has(formType)) info.forms.set(formType, fields);
  }
  return info;
}

function hasIdentity(info: DiscoInfo, category: string, type?: string): boolean {
  return info.identities.some((identity) => identity.category === category && (!type || identity.type === type));
}

function identityName(info: DiscoInfo, category?: string): string | null {
  return info.identities.find((identity) => !category || identity.category === category)?.name ?? null;
}

/**
 * A room description from its `disco#info` (XEP-0045 §6.4), or `null` when
 * the address does not describe itself as a room.
 */
export function roomDescriptionFromInfo(roomJid: string, info: DiscoInfo): Extract<RemoteDescription, { kind: "room" }> | null {
  if (!hasIdentity(info, "conference")) return null;
  const roomInfo = info.forms.get(NS_MUC_ROOMINFO);
  const spaceForm = info.forms.get(NS_SPACES);
  const parent = parseSpaceNodeIri(spaceForm?.get("parent") ?? roomInfo?.get("muc#roomconfig_pubsub") ?? null);
  return {
    kind: "room",
    roomJid,
    name: identityName(info, "conference"),
    description: roomInfo?.get("muc#roominfo_description") ?? null,
    flags: {
      membersOnly: info.features.has("muc_membersonly"),
      passwordProtected: info.features.has("muc_passwordprotected"),
      hidden: info.features.has("muc_hidden"),
      moderated: info.features.has("muc_moderated"),
      nonAnonymous: info.features.has("muc_nonanonymous"),
    },
    parent,
  };
}

/**
 * A Space description from its node's `disco#info`, or `null` when the node
 * is not a Space. XEP-0503 recognises a Space by `pubsub#type`; the
 * `urn:xmpp:spaces:0` feature is not required, since stock PubSub services
 * hold Spaces without advertising it.
 */
export function spaceDescriptionFromInfo(
  serviceJid: string,
  node: string,
  info: DiscoInfo,
): Extract<RemoteDescription, { kind: "space" }> | null {
  const metadata = info.forms.get(NS_PUBSUB_METADATA);
  if (metadata?.get("pubsub#type") !== NS_SPACES) return null;
  return {
    kind: "space",
    serviceJid,
    node,
    title: metadata.get("pubsub#title") ?? identityName(info, "pubsub"),
    description: metadata.get("pubsub#description") ?? null,
  };
}

/** Entries of a `disco#items` result, classified by their shape alone. */
export function parseDirectoryItems(doc: Document | Element, listedBy: DiscoInfo): RemoteDirectoryEntry[] {
  const query = firstElementNS(doc, NS_DISCO_ITEMS, "query");
  if (!query) return [];
  const isMucService = hasIdentity(listedBy, "conference") || listedBy.features.has(NS_MUC);
  const isPubsubService = hasIdentity(listedBy, "pubsub");
  const entries: RemoteDirectoryEntry[] = [];
  for (const item of childElements(query, "item", NS_DISCO_ITEMS)) {
    const jid = item.getAttribute("jid") ?? "";
    const node = item.getAttribute("node");
    const name = item.getAttribute("name")?.trim() || null;
    if (node && isPubsubService) {
      const serviceJid = canonicalDomain(jid);
      if (serviceJid) entries.push({ kind: "space", serviceJid, node, name });
    } else if (!node && jid.includes("@") && isMucService) {
      const roomJid = canonicalRoomJid(jid);
      if (roomJid) entries.push({ kind: "room", roomJid, name });
    } else if (!node && !jid.includes("@")) {
      const service = canonicalDomain(jid);
      if (service) entries.push({ kind: "service", jid: service, name });
    }
    if (entries.length >= MAX_LISTED) break;
  }
  return entries;
}

/** Ask the host what an address is, and list what a service holds. */
export async function describeCommunityAddress(
  xmpp: RawIqClient,
  address: CommunityAddress,
): Promise<RemoteDescription> {
  if (address.kind === "room") {
    const id = crypto.randomUUID();
    const info = parseDiscoInfo(await remoteIq(xmpp, discoInfoRequest(id, address.roomJid), id, address.roomJid));
    return roomDescriptionFromInfo(address.roomJid, info)
      ?? { kind: "other", jid: address.roomJid, name: identityName(info) };
  }
  if (address.kind === "space") {
    const id = crypto.randomUUID();
    const info = parseDiscoInfo(
      await remoteIq(xmpp, discoInfoRequest(id, address.serviceJid, address.node), id, address.serviceJid, address.node),
    );
    return spaceDescriptionFromInfo(address.serviceJid, address.node, info)
      ?? { kind: "other", jid: address.serviceJid, name: identityName(info) };
  }
  const infoId = crypto.randomUUID();
  const info = parseDiscoInfo(await remoteIq(xmpp, discoInfoRequest(infoId, address.serviceJid), infoId, address.serviceJid));
  const itemsId = crypto.randomUUID();
  const items = await remoteIq(xmpp, discoItemsRequest(itemsId, address.serviceJid), itemsId, address.serviceJid);
  return {
    kind: "directory",
    jid: address.serviceJid,
    name: identityName(info),
    entries: parseDirectoryItems(items, info),
  };
}

/** Parse a Space's `<items/>`, keeping rooms and the presence of images. */
export function parseSpaceContent(doc: Document | Element): RemoteSpaceContent {
  const content: RemoteSpaceContent = { rooms: [], hasAvatar: false, hasBanner: false, otherItems: 0 };
  const items = firstElementNS(doc, NS_PUBSUB, "items");
  if (!items) return content;
  const seen = new Set<string>();
  for (const item of childElements(items, "item", NS_PUBSUB)) {
    const id = item.getAttribute("id") ?? "";
    if (id === SPACE_AVATAR_ITEM) {
      content.hasAvatar = true;
      continue;
    }
    if (id === SPACE_BANNER_ITEM) {
      content.hasBanner = true;
      continue;
    }
    const conference = childElements(item, "conference", NS_BOOKMARKS)[0];
    const roomJid = conference ? canonicalRoomJid(id) : null;
    if (!conference || !roomJid || seen.has(roomJid)) {
      content.otherItems += 1;
      continue;
    }
    seen.add(roomJid);
    if (content.rooms.length >= MAX_LISTED) continue;
    content.rooms.push({
      roomJid,
      name: conference.getAttribute("name")?.trim() || null,
      suggestedAutojoin: ["true", "1"].includes(conference.getAttribute("autojoin") ?? ""),
    });
  }
  return content;
}

/** Read the rooms a Space lists (XEP-0503 §5.4). */
export async function fetchSpaceContent(xmpp: RawIqClient, serviceJid: string, node: string): Promise<RemoteSpaceContent> {
  const id = crypto.randomUUID();
  const xml = buildIqXml({
    type: "get",
    id,
    to: serviceJid,
    payload: { name: "pubsub", ns: NS_PUBSUB, children: [{ name: "items", attrs: { node } }] },
  });
  return parseSpaceContent(await remoteIq(xmpp, xml, id, serviceJid, node));
}

/** The subscription state a `<subscribe/>` result reports. */
export function parseSubscriptionState(doc: Document | Element): SpaceSubscriptionState {
  const subscription = firstElementNS(doc, NS_PUBSUB, "subscription");
  const state = subscription?.getAttribute("subscription");
  if (state === "pending" || state === "unconfigured") return state;
  // XEP-0060 §6.1.2 has a successful result name the subscription; a
  // result that names none still reports success.
  return "subscribed";
}

/**
 * Follow a Space: subscribe the account's bare JID to its node (XEP-0503
 * §5.3, XEP-0060 §6.1). The subscription admits nothing in the Space's rooms.
 */
export async function subscribeToSpace(
  xmpp: RawIqClient,
  serviceJid: string,
  node: string,
  ownBareJid: string,
): Promise<SpaceSubscriptionState> {
  const id = crypto.randomUUID();
  const xml = buildIqXml({
    type: "set",
    id,
    to: serviceJid,
    payload: { name: "pubsub", ns: NS_PUBSUB, children: [{ name: "subscribe", attrs: { node, jid: ownBareJid } }] },
  });
  return parseSubscriptionState(await remoteIq(xmpp, xml, id, serviceJid, node));
}

/**
 * Stop following a Space (XEP-0060 §6.2). Not being subscribed, and a host
 * that no longer knows the node, both leave no subscription behind and
 * count as done.
 */
export async function unsubscribeFromSpace(
  xmpp: RawIqClient,
  serviceJid: string,
  node: string,
  ownBareJid: string,
): Promise<void> {
  const id = crypto.randomUUID();
  const xml = buildIqXml({
    type: "set",
    id,
    to: serviceJid,
    payload: { name: "pubsub", ns: NS_PUBSUB, children: [{ name: "unsubscribe", attrs: { node, jid: ownBareJid } }] },
  });
  try {
    await remoteIq(xmpp, xml, id, serviceJid, node);
  } catch (error) {
    if (isNoSubscriptionLeft(error)) return;
    throw error;
  }
}

function isNoSubscriptionLeft(error: unknown): boolean {
  // `unexpected-request` is how an unsubscribe reports that there was no
  // subscription (XEP-0060 §6.2.3.2, with `<not-subscribed/>`).
  const { condition } = stanzaErrorContext(error);
  return condition === "item-not-found" || condition === "unexpected-request";
}
