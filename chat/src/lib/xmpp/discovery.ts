import type { WaddleClient } from "@waddle/xmpp-client-wasm";
import { normalizeChannelType } from "@/lib/channel-types";
import { reportError } from "@/lib/telemetry";
import { XMPP_ERROR_CONDITIONS } from "./types";
import type {
  DiscoveredChannel,
  DiscoveredSpace,
  DiscoveredTopology,
  RemoteRoomSuggestion,
  RoomCatalogFingerprintField,
  UnavailableRemoteRoom,
} from "./types";
import type { XmppCondition, XmppStreamReason } from "../telemetry-observations";
import { bareJidKey, barePeerJid, jidDomain, jidLocalpart } from "./jid";
import { qualifiedSpaceKey } from "./qualified-identity";
import {
  canonicalDomain,
  canonicalRoomJid,
  isRemoteRoomJid,
  parseSpaceNodeIri,
  roomHost,
  roomHostDomain,
  type HomeAddresses,
  type RoomHost,
} from "./remote-address";
import { FIELD_PIN_PERMISSION } from "./protocol-helpers";
import { stanzaErrorContext } from "./stanza-error-context";

const NS_FORUMS_0 = "urn:xmpp:forums:0";
const NS_MUC = "http://jabber.org/protocol/muc";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_METADATA = "http://jabber.org/protocol/pubsub#meta-data";
const NS_BOOKMARKS_1 = "urn:xmpp:bookmarks:1";
const NS_WADDLE_GROUP_DM = "urn:waddle:group-dm:0";
const NS_MUC_ROOMINFO = "http://jabber.org/protocol/muc#roominfo";
const DISCO_INFO_NS = "http://jabber.org/protocol/disco#info";
const DISCO_ITEMS_NS = "http://jabber.org/protocol/disco#items";
const DATAFORM_NS = "jabber:x:data";
const FIELD_FORUM_MODE = "muc#roomconfig_forum";

type DiscoveryRole = "owner" | "admin" | "moderator" | "member" | null;

type HybridClient = Partial<WaddleClient> & {
  send_raw_iq?: (xml: string) => Promise<string>;
  cancel_raw_iq?: (id: string) => Promise<void>;
};

export type DiscoInfoData = {
  features: string[];
  identities: Array<{ category?: string; type?: string; name?: string }>;
  fields: Map<string, string>;
  forms?: Array<{ formType: string | null; fields: Map<string, string> }>;
};

// RFC 6120 §8.2.3 requires IQ stanzas to be answered, but the requirement
// cannot be enforced on the wire — every modern XMPP client (Conversations,
// Gajim, Dino) defends against silently-dropped IQ responses with a bounded
// per-IQ timeout. 15s is short enough that a wedged component cannot stall
// topology discovery longer than one user heartbeat, long enough to survive
// realistic mobile/satellite RTTs.
const DISCO_IQ_TIMEOUT_MS_DEFAULT = 15_000;
let DISCO_IQ_TIMEOUT_MS = DISCO_IQ_TIMEOUT_MS_DEFAULT;

/**
 * Override the disco IQ timeout used by `sendDiscoInfo` / `sendDiscoItems`
 * / `sendPubsubItems`. Test-only backdoor — call with `null` to restore
 * the default. Lets integration tests verify the actual timeout wiring
 * without waiting 15s of real time per case. Production callers should
 * NEVER call this.
 */
export function __setDiscoIqTimeoutForTest(ms: number | null): void {
  DISCO_IQ_TIMEOUT_MS = ms ?? DISCO_IQ_TIMEOUT_MS_DEFAULT;
}

export class DiscoTimeoutError extends Error {
  constructor(
    public readonly to: string,
    public readonly node: string | undefined,
    public readonly timeoutMs: number,
  ) {
    // Report the actual budget the timer fired against — NOT the
    // module-level `DISCO_IQ_TIMEOUT_MS`, which may have been overridden
    // for this call via the optional `timeoutMs` argument to
    // `withIqTimeout` (e.g. from `__setDiscoIqTimeoutForTest` in tests).
    super(`disco IQ to ${to}${node ? ` (node=${node})` : ""} timed out after ${timeoutMs}ms`);
    this.name = "DiscoTimeoutError";
  }
}

/**
 * Wrap an in-flight IQ Promise with a bounded timeout.
 *
 * Known limitation — driver-side pending_iqs retention: rejecting the JS
 * Promise here does NOT remove the entry from the wasm driver's pending_iqs
 * HashMap (server/crates/waddle-xmpp-client-wasm/src/driver.rs:222). The
 * entry stays until either a (possibly never-arriving) response or stream
 * disconnect, at which point `pending_iqs.drain()` clears it. Per-stream
 * leak is bounded (~100 B × ~7 IQs per topology rebuild) and fully reclaimed
 * on reconnect. The proper fix is a `cancel_iq(id)` driver command — tracked
 * as a follow-up; not blocking the channel-rendering bug this PR closes.
 */
export function withIqTimeout<T>(
  raw: Promise<T>,
  to: string,
  node: string | undefined,
  timeoutMs?: number,
  options: { cancel?: () => void | Promise<void> } = {},
): Promise<T> {
  // Read `DISCO_IQ_TIMEOUT_MS` at call time, not at function-definition
  // time, so `__setDiscoIqTimeoutForTest` correctly overrides for the
  // duration of a test case.
  const effectiveTimeout = timeoutMs ?? DISCO_IQ_TIMEOUT_MS;
  let didTimeout = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      didTimeout = true;
      reject(new DiscoTimeoutError(to, node, effectiveTimeout));
    }, effectiveTimeout);
  });
  return Promise.race([raw, timeout]).finally(async () => {
    if (timer !== undefined) clearTimeout(timer);
    if (didTimeout) {
      try {
        await options.cancel?.();
      } catch (err) {
        console.warn("[disco] raw IQ cancellation failed", { to, node, err });
      }
    }
  });
}

function cancelRawIqOnTimeout(xmpp: HybridClient, id: string): { cancel?: () => Promise<void> } {
  return typeof xmpp.cancel_raw_iq === "function"
    ? { cancel: () => xmpp.cancel_raw_iq!(id) }
    : {};
}

function parseXml(xml: string): Document {
  return new DOMParser().parseFromString(xml, "text/xml");
}

function xmlAttr(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("\"", "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function elementChildren(element: Element, localName: string, namespace: string): Element[] {
  return Array.from(element.children).filter((child) => child.localName === localName && child.namespaceURI === namespace);
}

function textContent(element: Element | null | undefined): string | null {
  const value = element?.textContent?.trim();
  return value ? value : null;
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

export function isGroupDmDiscoInfo(info: DiscoInfoData | null | undefined): boolean {
  return hasDiscoFeature(info, NS_WADDLE_GROUP_DM);
}

function hasDiscoFeature(info: DiscoInfoData | null | undefined, feature: string): boolean {
  return info?.features.some((candidate) => candidate === feature) ?? false;
}

function hasDiscoIdentity(info: DiscoInfoData | null | undefined, category: string, type: string): boolean {
  return info?.identities.some((identity) => identity.category === category && identity.type === type) ?? false;
}

function formField(info: DiscoInfoData, formType: string, fieldName: string): string | null {
  return info.forms?.find((form) => form.formType === formType)?.fields.get(fieldName) ?? null;
}

function isSpacesServiceInfo(info: DiscoInfoData | null | undefined): boolean {
  return hasDiscoFeature(info, NS_SPACES_0)
    && hasDiscoFeature(info, NS_PUBSUB)
    && hasDiscoIdentity(info, "pubsub", "service");
}

function isSpacesNodeInfo(info: DiscoInfoData | null | undefined): boolean {
  return !!info
    && hasDiscoFeature(info, NS_SPACES_0)
    && hasDiscoFeature(info, NS_PUBSUB)
    && hasDiscoIdentity(info, "pubsub", "leaf")
    && formField(info, NS_PUBSUB_METADATA, "pubsub#type") === NS_SPACES_0;
}

/**
 * The Space a room names as its parent, as a node on `spacesServiceJid`.
 * A Space is identified by its service and node together: a parent on any
 * other service, or one that names no service, is not one of the home
 * deployment's Spaces, whatever its node is called.
 */
function roomParentSpaceId(info: DiscoInfoData, spacesServiceJid: string | undefined): string | null {
  const home = spacesServiceJid ? canonicalDomain(barePeerJid(spacesServiceJid)) : null;
  if (!home) return null;
  const parent = parseSpaceNodeIri(formField(info, NS_SPACES_0, "parent"))
    ?? parseSpaceNodeIri(formField(info, NS_MUC_ROOMINFO, "muc#roomconfig_pubsub"));
  return parent && parent.serviceJid === home ? parent.node : null;
}

function logDiscoFailure(kind: "info" | "items" | "pubsub", to: string, node: string | undefined, err: unknown): void {
  if (err instanceof DiscoTimeoutError) {
    // Timeouts are the diagnostic signal we actually care about when
    // channels fail to render — keep them structured so consumers can
    // grep / aggregate by `to`.
    console.warn(`[disco] ${kind} timeout`, { to: err.to, node: err.node });
    reportError({
      kind: "xmpp.stream",
      reason: `disco-${kind}-timeout`,
      recoverable: true,
      errorSource: "local-timeout",
    });
    return;
  }
  console.error(`[disco] ${kind} FAILED`, { to, node, err });
  const context = stanzaErrorContext(err);
  // Beacon only attributable failures (server stanza error or the timeout
  // branch above). Condition-less rejections — chiefly "client is
  // disconnected" — reject every in-flight disco IQ of a topology load at
  // once on a connection flap, and beaconing each would flood Faro with
  // one error per room. Those stay console-only.
  if (!context.condition) return;
  // Allowlist the condition like `telemetryCondition()` does for
  // XmppErrorEvents: a non-standard element name must not mint
  // high-cardinality `disco-*-*` details. The beacon exception is a
  // fresh Error carrying only the detail — never the raw rejection,
  // whose message embeds the uncapped server `<text/>`.
  const condition = (XMPP_ERROR_CONDITIONS.has(context.condition) ? context.condition : "unknown") as XmppCondition;
  reportError({
    kind: "xmpp.stream",
    reason: `disco-${kind}-${condition}` as XmppStreamReason,
    recoverable: true,
    ...(context.errorType ? { errorType: context.errorType } : {}),
    condition,
    errorSource: "server",
  });
}

async function sendDiscoInfo(xmpp: HybridClient, to: string, node?: string): Promise<DiscoInfoData | null> {
  if (!xmpp.send_raw_iq) return null;
  const id = crypto.randomUUID();
  const nodeAttr = node ? ` node="${xmlAttr(node)}"` : "";
  const toAttr = xmlAttr(to);
  let responseXml: string;
  try {
    responseXml = await withIqTimeout(
      xmpp.send_raw_iq(`<iq type="get" id="${id}" to="${toAttr}"><query xmlns="${DISCO_INFO_NS}"${nodeAttr}/></iq>`),
      to,
      node,
      undefined,
      cancelRawIqOnTimeout(xmpp, id),
    );
  } catch (err) {
    logDiscoFailure("info", to, node, err);
    throw err;
  }
  const query = parseXml(responseXml).getElementsByTagNameNS(DISCO_INFO_NS, "query")[0];
  if (!query) return null;
  const fields = new Map<string, string>();
  const forms: DiscoInfoData["forms"] = [];
  for (const form of elementChildren(query, "x", DATAFORM_NS)) {
    const formFields = new Map<string, string>();
    for (const field of elementChildren(form, "field", DATAFORM_NS)) {
      const name = field.getAttribute("var");
      if (!name) continue;
      const value = textContent(elementChildren(field, "value", DATAFORM_NS)[0] ?? field.querySelector("value"));
      if (value) fields.set(name, value);
      if (value) formFields.set(name, value);
    }
    forms.push({ formType: formFields.get("FORM_TYPE") ?? null, fields: formFields });
  }
  return {
    features: elementChildren(query, "feature", DISCO_INFO_NS).map((feature) => feature.getAttribute("var") ?? "").filter(Boolean),
    identities: elementChildren(query, "identity", DISCO_INFO_NS).map((identity) => ({ category: identity.getAttribute("category") ?? undefined, type: identity.getAttribute("type") ?? undefined, name: identity.getAttribute("name") ?? undefined })),
    fields,
    forms,
  };
}

async function sendDiscoItems(xmpp: HybridClient, to: string, node?: string): Promise<Array<{ jid?: string; name?: string; node?: string }>> {
  if (!xmpp.send_raw_iq) return [];
  const id = crypto.randomUUID();
  const nodeAttr = node ? ` node="${xmlAttr(node)}"` : "";
  const xml = `<iq type="get" id="${id}" to="${xmlAttr(to)}"><query xmlns="${DISCO_ITEMS_NS}"${nodeAttr}/></iq>`;
  let responseXml: string;
  try {
    responseXml = await withIqTimeout(xmpp.send_raw_iq(xml), to, node, undefined, cancelRawIqOnTimeout(xmpp, id));
  } catch (err) {
    logDiscoFailure("items", to, node, err);
    throw err;
  }
  const doc = parseXml(responseXml);
  const query = doc.getElementsByTagNameNS(DISCO_ITEMS_NS, "query")[0];
  if (!query) return [];
  return Array.from(query.getElementsByTagNameNS(DISCO_ITEMS_NS, "item"))
    .filter((item) => item.parentNode === query)
    .map((item) => ({ jid: item.getAttribute("jid") ?? undefined, name: item.getAttribute("name") ?? undefined, node: item.getAttribute("node") ?? undefined }));
}

type PubsubBookmarkItem = {
  jid?: string;
  name?: string;
  /** XEP-0402 `autojoin`; omitted attribute means false for bookmarks. */
  autojoin?: boolean;
};

async function sendPubsubItems(xmpp: HybridClient, to: string, node: string): Promise<PubsubBookmarkItem[]> {
  if (!xmpp.send_raw_iq) return [];
  const id = crypto.randomUUID();
  const toAttr = xmlAttr(to);
  const nodeAttr = xmlAttr(node);
  let responseXml: string;
  try {
    responseXml = await withIqTimeout(
      xmpp.send_raw_iq(`<iq type="get" id="${id}" to="${toAttr}"><pubsub xmlns="${NS_PUBSUB}"><items node="${nodeAttr}"/></pubsub></iq>`),
      to,
      node,
      undefined,
      cancelRawIqOnTimeout(xmpp, id),
    );
  } catch (err) {
    logDiscoFailure("pubsub", to, node, err);
    throw err;
  }
  const doc = parseXml(responseXml);
  const items = doc.getElementsByTagNameNS(NS_PUBSUB, "items")[0];
  if (!items) return [];
  return Array.from(items.getElementsByTagNameNS(NS_PUBSUB, "item"))
    .filter((item) => item.parentNode === items)
    .flatMap((item) => {
      const conference = elementChildren(item, "conference", NS_BOOKMARKS_1)[0];
      const roomJid = item.getAttribute("id") ?? undefined;
      if (!conference || !roomJid) return [];
      return {
        jid: roomJid,
        name: conference.getAttribute("name") ?? undefined,
        autojoin: parseBooleanValue(conference.getAttribute("autojoin")) ?? false,
      };
    });
}

export function spacesServiceDomain(jid: string): string { return `spaces.${jidDomain(jid)}`; }
export function mucServiceDomain(jid: string): string { return `muc.${jidDomain(jid)}`; }

/**
 * Whether a component's `disco#info` shows it is a MUC service: it lists
 * the XEP-0045 feature. An identity alone is not enough, and a
 * `conference/audio-video` identity (a call mixer such as `calls.<domain>`)
 * never names the MUC service.
 */
function isMucServiceInfo(info: DiscoInfoData | null): boolean {
  return hasDiscoFeature(info, NS_MUC) && !hasDiscoIdentity(info, "conference", "audio-video");
}

/**
 * The home deployment's MUC and Spaces services, with whether the MUC
 * service was identified by its feature rather than assumed, and the
 * components the home server lists (`null` when the list was not read).
 *
 * The MUC service is the listed component that advertises the XEP-0045
 * feature. A component with only a `conference/text` identity is used as
 * the service's address when none advertises the feature, but does not
 * count as identified: only an identified service lets rooms elsewhere be
 * told apart from home rooms, and with them left or dropped.
 */
async function discoverComponentServices(
  xmpp: HybridClient,
  domain: string,
  jid: string,
): Promise<{
  services: { muc: string; spaces: string };
  mucDiscovered: boolean;
  components: string[] | null;
}> {
  const fallback = { muc: mucServiceDomain(jid), spaces: spacesServiceDomain(jid) };
  try {
    const items = await sendDiscoItems(xmpp, domain);
    const candidates = items.map((item) => item.jid).filter((value): value is string => !!value);
    // `allSettled` (not `all`): a single hung component must not wedge the
    // whole topology fan-out. A rejected entry (timeout, stanza error, parse
    // failure) maps to `info: null`, which is the same shape the existing
    // try/catch produced — every downstream consumer already treats `null` as
    // "service absent" and falls back to the conventional domain.
    //
    // Caps cache safety (XEP-0115): an `info: null` here represents IQ
    // failure, NOT "this entity has zero features". A future consumer that
    // caches disco-info by ver hash MUST NOT persist this fallback shape —
    // doing so would poison the cache against a recovering component.
    const settled = await Promise.allSettled(candidates.map(async (serviceJid) => ({
      serviceJid,
      info: await sendDiscoInfo(xmpp, serviceJid),
    })));
    const serviceInfo = settled.map((entry, index) =>
      entry.status === "fulfilled"
        ? entry.value
        : { serviceJid: candidates[index]!, info: null },
    );
    const identifiedMuc = serviceInfo.find(({ info }) => isMucServiceInfo(info))?.serviceJid;
    const textConference = serviceInfo.find(({ info }) => hasDiscoIdentity(info, "conference", "text"))?.serviceJid;
    const muc = identifiedMuc ?? textConference ?? fallback.muc;
    // A generic pubsub identity is not enough: the extensions component is
    // also pubsub. Keep the conventional Spaces fallback unless a component
    // explicitly advertises the XEP-0503 namespace.
    const spaces = serviceInfo.find(({ info }) => isSpacesServiceInfo(info))?.serviceJid ?? fallback.spaces;
    return {
      services: { muc, spaces },
      mucDiscovered: identifiedMuc !== undefined,
      components: candidates.map((candidate) => barePeerJid(candidate)),
    };
  } catch {
    return { services: fallback, mucDiscovered: false, components: null };
  }
}

/**
 * Local id of a room known only from a bookmark. A room of the home MUC
 * service keeps its localpart, as rooms of the home catalog do, so existing
 * routes and stored state still resolve. A room anywhere else is identified
 * by its canonical bare JID: a localpart names a room only relative to its
 * service, and a remote `general` must never take the id of the home
 * `general`. A localpart cannot contain `@`, so the two forms never collide.
 */
export function channelIdForRoom(roomJid: string, homeMucService: string): string {
  const home = canonicalDomain(barePeerJid(homeMucService));
  const room = canonicalRoomJid(barePeerJid(roomJid));
  if (room && home && isRemoteRoomJid(room, { accountDomain: "", mucService: home })) return room;
  return jidLocalpart(roomJid);
}

function channelFromRoom(
  room: { jid: string; name?: string; channel_type?: string },
  position: number,
  id: string = jidLocalpart(room.jid),
): DiscoveredChannel {
  return { id, name: room.name || id, jid: room.jid, channelType: normalizeChannelType(room.channel_type || "text"), position };
}

/** #422: extract the room's pin policy from a parsed disco-info form
 * field map. Returns the typed `PinPermission` when the field is
 * present and recognized, `undefined` otherwise (consumer defaults to
 * `admins-only`). Exported for unit testing. */
export function pinPermissionFromDiscoFields(fields: Map<string, string>): "admins-only" | "anyone" | undefined {
  const raw = fields.get(FIELD_PIN_PERMISSION);
  return raw === "anyone" || raw === "admins-only" ? raw : undefined;
}

/** #422: end-to-end hydration helper — given a parsed disco-info
 * payload, apply every field to the channel record so callers (and
 * tests) can verify the full mapping without going through DOMParser.
 * `hydrateRoomInfo` is the IO-boundary wrapper; this function is the
 * pure transform. */
export function applyDiscoInfoToChannel(
  room: DiscoveredChannel,
  info: DiscoInfoData,
  spacesServiceJid?: string,
): DiscoveredChannel {
  const parentSpaceId = roomParentSpaceId(info, spacesServiceJid);
  const pinPermission = pinPermissionFromDiscoFields(info.fields);
  return {
    ...room,
    channelType: channelTypeFromInfo(info),
    ...(isGroupDmDiscoInfo(info) ? { isGroupDm: true } : {}),
    ...(parentSpaceId ? { spaceId: parentSpaceId, standalone: false } : {}),
    ...(info.features.length ? { features: info.features } : {}),
    ...(pinPermission ? { pinPermission } : {}),
  };
}

type RoomInfoHydration = {
  room: DiscoveredChannel;
  fingerprintComplete: boolean;
};

async function hydrateRoomInfo(
  xmpp: HybridClient,
  room: DiscoveredChannel,
  spacesServiceJid?: string,
): Promise<RoomInfoHydration> {
  if (!room.jid) return { room, fingerprintComplete: false };
  // No internal try/catch: callers wrap this in `Promise.allSettled` and
  // map rejected entries to the unhydrated `channelFromRoom` record, so a
  // local catch here would make the outer rejection branch unreachable
  // dead code. Single resilience layer (one site to reason about), not
  // two redundant ones.
  const info = await sendDiscoInfo(xmpp, room.jid);
  if (!info) return { room, fingerprintComplete: false };
  return {
    room: applyDiscoInfoToChannel(room, info, spacesServiceJid),
    fingerprintComplete: true,
  };
}

export async function discoverChannels(xmpp: HybridClient, jid: string): Promise<DiscoveredChannel[]> {
  const spacesService = spacesServiceDomain(jid);
  const spaces = await sendDiscoItems(xmpp, spacesService);
  const spaceNode = spaces[0]?.node;
  if (!spaceNode) return [];
  const info = await sendDiscoInfo(xmpp, spacesService, spaceNode);
  if (!isSpacesNodeInfo(info)) return [];
  const items = await sendPubsubItems(xmpp, spacesService, spaceNode);
  const settled = await Promise.allSettled(
    items.map((item, position) =>
      hydrateRoomInfo(xmpp, channelFromRoom({ jid: item.jid ?? "", name: item.name }, position), spacesService),
    ),
  );
  const hydrated = settled.map((entry, index) =>
    entry.status === "fulfilled"
      ? entry.value.room
      : channelFromRoom({ jid: items[index]!.jid ?? "", name: items[index]!.name }, index),
  );
  return hydrated.map((room, position) => ({ id: room.id, jid: room.jid, name: room.name, channelType: room.channelType, position }));
}

/**
 * Feature the home server lists in its domain's `disco#info` while it
 * federates with other servers. Until it does, nothing on other servers is
 * offered or joined: rooms the user bookmarked there are listed as
 * unavailable, and Spaces there are neither read nor renewed.
 */
export const HOME_FEDERATION_FEATURE = "urn:waddle:s2s:0";

interface DiscoverTopologyOptions {
  /**
   * Whether the home server advertised federation at the last discovery
   * that could read its `disco#info`; used when this discovery cannot.
   */
  lastKnownFederation?: boolean | null;
}

export async function discoverTopology(
  xmpp: HybridClient,
  jid: string,
  options: DiscoverTopologyOptions = {},
): Promise<DiscoveredTopology> {
  const domain = jidDomain(jid);
  const { services, mucDiscovered, components } = await discoverComponentServices(xmpp, domain, jid);
  // Only an identified MUC service tells home rooms from remote ones: the
  // conventional name may not be this deployment's service. A component
  // the home server lists is home whether or not it answered its own
  // `disco#info`.
  const home: HomeAddresses = {
    accountDomain: domain,
    mucService: mucDiscovered ? services.muc : null,
    spacesService: services.spaces,
    components,
  };
  let rooms: DiscoveredChannel[] = [];
  let roomCatalogComplete = true;
  let bookmarkCatalogComplete = true;
  let mucCatalogComplete = true;
  let userBookmarksComplete = true;
  // Space items and bookmarks whose room this discovery could not place.
  let unplacedRooms = 0;
  const bookmarkedRooms = new Map<string, { spaceId?: string; autojoin: boolean; name?: string }>();
  const remoteBookmarks = new Map<string, { autojoin: boolean; name?: string }>();
  const spaceBookmarkedRoomJids = new Set<string>();
  const userBookmarkedRoomJids = new Set<string>();
  const spaceItems: Array<{ spaceId: string; roomJid: string; autojoin: boolean; name?: string }> = [];
  const userBookmarkItems: Array<{ roomJid: string; autojoin: boolean; name?: string }> = [];
  const remoteRoomSuggestions: RemoteRoomSuggestion[] = [];
  const unavailableRemoteRooms: UnavailableRemoteRoom[] = [];
  const spaces: DiscoveredSpace[] = [];
  const seenSpaceKeys = new Set<string>();
  let serverRole: DiscoveryRole = null;

  const serverInfoResult = await sendDiscoInfo(xmpp, domain).catch(() => null);
  if (serverInfoResult) {
    serverRole = parseDiscoveryRole(serverInfoResult.fields.get("waddle#server_affiliation") ?? null);
  }
  const advertisedFederation = serverInfoResult ? hasDiscoFeature(serverInfoResult, HOME_FEDERATION_FEATURE) : null;
  const federation = advertisedFederation ?? options.lastKnownFederation ?? false;

  try {
    const spaceList = await sendDiscoItems(xmpp, services.spaces);
    for (const item of spaceList) {
      if (!item.node) continue;
      const spaceId = item.node;
      const spaceKey = qualifiedSpaceKey(services.spaces, item.node);
      if (seenSpaceKeys.has(spaceKey)) continue;
      let role = serverRole;
      try {
        const info = await sendDiscoInfo(xmpp, services.spaces, item.node);
        if (!isSpacesNodeInfo(info)) continue;
        if (info) role = parseDiscoveryRole(info.fields.get("pubsub#affiliation") ?? null) ?? serverRole;
      } catch {
        roomCatalogComplete = false;
        bookmarkCatalogComplete = false;
        continue;
      }
      try {
        const bookmarks = await sendPubsubItems(xmpp, services.spaces, item.node);
        for (const bookmark of bookmarks) {
          if (bookmark.jid) {
            spaceItems.push({
              spaceId,
              roomJid: barePeerJid(bookmark.jid),
              autojoin: bookmark.autojoin ?? false,
              name: bookmark.name,
            });
          }
        }
      } catch {
        roomCatalogComplete = false;
        bookmarkCatalogComplete = false;
      }
      seenSpaceKeys.add(spaceKey);
      spaces.push({ id: spaceId, serviceJid: services.spaces, name: item.name ?? spaceId, role });
    }
  } catch {
    roomCatalogComplete = false;
    bookmarkCatalogComplete = false;
  }

  try {
    const userBookmarks = await sendPubsubItems(xmpp, barePeerJid(jid), NS_BOOKMARKS_1);
    for (const bookmark of userBookmarks) {
      if (!bookmark.jid) continue;
      userBookmarkItems.push({
        roomJid: barePeerJid(bookmark.jid),
        autojoin: bookmark.autojoin ?? false,
        name: bookmark.name,
      });
    }
  } catch {
    roomCatalogComplete = false;
    bookmarkCatalogComplete = false;
    userBookmarksComplete = false;
  }

  // Narrow try/catch JUST around `sendDiscoItems`: a wedged muc service
  // (which now times out via `withIqTimeout` instead of hanging forever)
  // degrades the sidebar to "no rooms" rather than failing the whole
  // topology load and abandoning already-resolved spaces work. Subsequent
  // hydration uses `Promise.allSettled` so a single broken room cannot
  // poison the others; `hydrateRoomInfo` already self-catches, so the
  // `allSettled` is belt-and-braces against a future regression there.
  // Errors in barePeerJid, channelFromRoom, or the final rooms.map are
  // intentionally left to propagate — those are programmer errors, not
  // wire-state errors, and silently swallowing them would mask bugs.
  let mucRooms: Array<{ jid?: string; name?: string }> = [];
  try {
    mucRooms = await sendDiscoItems(xmpp, services.muc);
  } catch (err) {
    // sendDiscoItems already logged via logDiscoFailure; here we just
    // continue with an empty room list. The cause is observable in the
    // console for diagnostics.
    roomCatalogComplete = false;
    mucCatalogComplete = false;
    void err;
  }
  const catalogRoomKeys = new Set(
    mucRooms.flatMap((room) => (room.jid ? [bareJidKey(room.jid)] : [])),
  );
  // A room is of this deployment when the home MUC catalog lists it or its
  // address is on a home service. A Space's say-so is never evidence: any
  // Space publisher could otherwise make every member's client join a room
  // on a server of its choosing.
  const hostOf = (roomJid: string): RoomHost =>
    catalogRoomKeys.has(bareJidKey(roomJid)) ? "home" : roomHost(roomJid, home);

  // A home Space lists rooms of this deployment. One naming a room
  // elsewhere is no part of the Space here: it is offered as a suggestion
  // the user can look up, and is never listed in the Space or joined on
  // the Space's word. While the home MUC service is not discovered, such
  // an item cannot be placed and waits for a later discovery.
  for (const item of spaceItems) {
    const host = hostOf(item.roomJid);
    if (host === "home") {
      spaceBookmarkedRoomJids.add(item.roomJid);
      bookmarkedRooms.set(item.roomJid, { spaceId: item.spaceId, autojoin: item.autojoin, name: item.name });
    } else if (host === "remote") {
      remoteRoomSuggestions.push({
        spaceId: item.spaceId,
        roomJid: canonicalRoomJid(item.roomJid) ?? item.roomJid,
        ...(item.name ? { name: item.name } : {}),
      });
    } else {
      unplacedRooms += 1;
      roomCatalogComplete = false;
      bookmarkCatalogComplete = false;
    }
  }

  // The user's own XEP-0402 bookmarks. A bookmark of a room on another
  // server is the only record that lists or joins it, and only while the
  // home server federates; until then it is listed as unavailable. While
  // the home MUC service is not discovered, a bookmark whose room is not
  // shown to be of this deployment cannot be placed: it waits for a later
  // discovery, unasked and unjoined, since asking or joining it would
  // reach another server before federation is known to be on.
  for (const bookmark of userBookmarkItems) {
    const roomJid = bookmark.roomJid;
    userBookmarkedRoomJids.add(roomJid);
    const host = hostOf(roomJid);
    if (host === "unknown") {
      unplacedRooms += 1;
      roomCatalogComplete = false;
      bookmarkCatalogComplete = false;
      continue;
    }
    if (host === "remote") {
      const canonical = canonicalRoomJid(roomJid) ?? roomJid;
      if (!federation) {
        unavailableRemoteRooms.push({ roomJid: canonical, ...(bookmark.name ? { name: bookmark.name } : {}) });
      } else {
        remoteBookmarks.set(canonical, { autojoin: bookmark.autojoin, ...(bookmark.name ? { name: bookmark.name } : {}) });
      }
      continue;
    }
    const existing = bookmarkedRooms.get(roomJid);
    bookmarkedRooms.set(roomJid, {
      ...existing,
      autojoin: bookmark.autojoin,
      name: bookmark.name ?? existing?.name,
    });
  }

  const roomItems: Array<{ jid?: string; name?: string; bookmarkOnly?: boolean; remote?: boolean }> =
    mucRooms.map((room) => ({ ...room, bookmarkOnly: false }));
  const discoveredRoomJids = new Set(
    roomItems
      .map((room) => room.jid ? barePeerJid(room.jid) : null)
      .filter((roomJid): roomJid is string => roomJid !== null),
  );
  for (const [roomJid, bookmark] of bookmarkedRooms.entries()) {
    if (!discoveredRoomJids.has(roomJid)) {
      roomItems.push({ jid: roomJid, name: bookmark.name, bookmarkOnly: true, remote: false });
    }
  }
  for (const [roomJid, bookmark] of remoteBookmarks.entries()) {
    roomItems.push({ jid: roomJid, name: bookmark.name, bookmarkOnly: true, remote: true });
  }
  // A bookmarked room on another server is listed from the bookmark alone.
  // Asking its host here would put a federated round trip (and, while that
  // server is down, a full timeout) into every structure load, and its
  // answer is no part of the home catalog: joining the room asks the host.
  const hydratedSettled = await Promise.allSettled(
    roomItems.map((room, position) => {
      const roomJid = room.jid ?? "";
      const id = room.remote ? channelIdForRoom(roomJid, services.muc) : undefined;
      const channel = channelFromRoom({ jid: roomJid, name: room.name }, position, id);
      return room.remote
        ? Promise.resolve<RoomInfoHydration>({
          room: { ...channel, remoteHost: roomHostDomain(roomJid) ?? undefined },
          fingerprintComplete: false,
        })
        : hydrateRoomInfo(xmpp, channel, services.spaces);
    }),
  );
  if (hydratedSettled.some(
    (entry, index) =>
      !roomItems[index]!.remote
      && (entry.status === "rejected" || !entry.value.fingerprintComplete)
  )) {
    roomCatalogComplete = false;
  }
  const fingerprintCompleteRoomJids = new Set(
    hydratedSettled.flatMap((entry) => {
      if (entry.status !== "fulfilled" || !entry.value.fingerprintComplete) return [];
      const roomJid = entry.value.room.jid ? barePeerJid(entry.value.room.jid) : "";
      return roomJid ? [roomJid] : [];
    }),
  );
  const hydrated = hydratedSettled.flatMap((entry, index) => {
    const source = roomItems[index]!;
    if (entry.status === "fulfilled") {
      if (
        source.bookmarkOnly
        && !source.remote
        && !entry.value.room.features?.some((feature) => feature === NS_MUC)
      ) {
        return [];
      }
      return [entry.value.room];
    }
    if (source.bookmarkOnly) return [];
    return [channelFromRoom({ jid: source.jid ?? "", name: source.name }, index)];
  });
  const validSpaceIds = new Set(spaces.map((space) => space.id));
  rooms = hydrated.map((room, position) => {
    const roomJid = room.jid ? barePeerJid(room.jid) : "";
    const bookmark: { spaceId?: string; autojoin: boolean; name?: string } | undefined = roomJid
      ? bookmarkedRooms.get(roomJid) ?? remoteBookmarks.get(roomJid)
      : undefined;
    // A room's own claim of a parent Space places it there only when the
    // room is of this deployment: a room elsewhere naming a home Space
    // claims nothing here.
    const parentSpaceId = room.spaceId && validSpaceIds.has(room.spaceId) && !!roomJid && hostOf(roomJid) === "home"
      ? room.spaceId
      : undefined;
    const { spaceId: _discardUnverifiedSpaceId, standalone: _discardUnverifiedStandalone, ...roomWithoutSpace } = room;
    return {
      ...roomWithoutSpace,
      position,
      isBookmarked: !!bookmark,
      ...(bookmark?.spaceId
        ? { spaceId: bookmark.spaceId, standalone: false, autojoin: bookmark.autojoin }
        : parentSpaceId
          ? { spaceId: parentSpaceId, standalone: false, autojoin: true }
        : bookmark
          ? { autojoin: bookmark.autojoin }
          : { autojoin: true }),
    };
  });

  const roomSpaceIds = new Map(rooms.flatMap((room) => room.jid ? [[barePeerJid(room.jid), room.spaceId]] as const : []));
  const authoritativeRoomFingerprints = rooms.flatMap((room) => {
    const roomJid = room.jid ? barePeerJid(room.jid) : "";
    if (!roomJid || !fingerprintCompleteRoomJids.has(roomJid)) return [];
    if (bookmarkCatalogComplete) {
      return [{
        roomKey: bareJidKey(roomJid),
        fields: [
          "spaceId",
          "autojoin",
          "isGroupDm",
          "isBookmarked",
        ] satisfies RoomCatalogFingerprintField[],
      }];
    }
    const hasSpaceBookmark = spaceBookmarkedRoomJids.has(roomJid);
    const hasUserBookmark = userBookmarkedRoomJids.has(roomJid);
    if (!hasSpaceBookmark && !hasUserBookmark) return [];
    const fields: RoomCatalogFingerprintField[] = [
      "isGroupDm",
      "isBookmarked",
    ];
    if (hasSpaceBookmark) fields.push("spaceId");
    if (hasUserBookmark || (hasSpaceBookmark && userBookmarksComplete)) {
      fields.push("autojoin");
    }
    return [{ roomKey: bareJidKey(roomJid), fields }];
  });
  return {
    spaces,
    rooms: rooms.map((room, position) => ({ ...room, position, ...(room.jid && roomSpaceIds.get(barePeerJid(room.jid)) ? { standalone: false } : { standalone: room.standalone ?? true }) })),
    roomCatalogComplete,
    mucCatalogRead: mucCatalogComplete,
    unplacedRooms,
    roomReconciliationAuthority: {
      absentRoomKeysAuthoritative:
        roomCatalogComplete && bookmarkCatalogComplete && mucCatalogComplete,
      roomFingerprints: authoritativeRoomFingerprints,
    },
    serverRole,
    services: { ...services, mucDiscovered, ...(components ? { components } : {}) },
    federation: advertisedFederation,
    remoteRoomSuggestions,
    unavailableRemoteRooms,
  };
}
