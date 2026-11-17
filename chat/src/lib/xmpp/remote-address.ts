// Addresses of communities hosted on other servers.
//
// A user reaches a remote community by typing its address: a room JID, a
// Space's node IRI or a service domain, optionally as an RFC 5122 `xmpp:`
// URI. This module turns that input into one typed, canonical address and
// says which addresses belong to the home deployment. It decides nothing
// about access: what an address is, and whether it may be used, is the
// hosting server's answer (see `remote-community.ts`).
//
// Canonical form follows the server's identity profile so an address the
// client stores, such as a XEP-0402 bookmark item id, is the text the
// server accepts: one trailing dot is removed from the domain, the domain is
// lower case and in A-label form, and a room localpart is lower case.

/** Kind of a canonical domain, as the server's identity profile classifies it. */
export type DomainKind = "ipv6" | "ipv4" | "numeric-ipv4" | "single-label" | "dns";

export type CommunityAddress =
  | { kind: "room"; roomJid: string }
  | { kind: "space"; serviceJid: string; node: string }
  | { kind: "service"; serviceJid: string };

export type CommunityAddressError =
  /** Nothing usable was entered. */
  | "empty"
  /** A full JID: rooms are addressed by their bare JID. */
  | "resource"
  /** Not a JID, node IRI or domain this client can read. */
  | "malformed"
  /** An IP address, numeric or single-label domain, which servers refuse for federation. */
  | "refused-domain";

export type ParsedCommunityAddress =
  | { ok: true; address: CommunityAddress }
  | { ok: false; error: CommunityAddressError };

/**
 * The home deployment's own addresses: its account domain and the services
 * it discovered. `mucService` is set only once the home MUC service was
 * discovered; a conventional name such as `muc.<domain>` is never put here,
 * since it may name another server.
 */
export interface HomeAddresses {
  accountDomain: string;
  mucService?: string | null;
  spacesService?: string | null;
  /**
   * The components the home server lists as its own items (XEP-0030
   * `disco#items` of the account domain), as last read. A room on one of
   * them is of this deployment even when the component did not answer its
   * own `disco#info`.
   */
  components?: readonly string[] | null;
}

/**
 * Where a room is hosted, as far as the home deployment's discovered
 * services tell:
 * - `home`: on the account domain, a discovered home service or a component
 *   the home server lists;
 * - `remote`: anywhere else, once the home MUC service is known;
 * - `unknown`: anywhere else while the home MUC service is not yet
 *   discovered. Callers decide what waiting means for them: timers and
 *   copy treat it as home, while nothing that grants a room a place in a
 *   home Space or joins it on someone else's word treats it as either.
 */
export type RoomHost = "home" | "remote" | "unknown";

const NON_ASCII = /[^\u0000-\u007f]/;
const DNS_LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
const IPV4_ADDRESS = /^(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)$/;
const NUMERIC_LABEL = /^(?:\d+|0x[0-9a-f]*)$/;

/**
 * Canonical text of a domain, or `null` when it is not a domain. Removes one
 * trailing dot, maps an internationalized name to its A-label form and lower
 * cases the rest. An IP literal keeps its bracketed text.
 */
export function canonicalDomain(raw: string): string | null {
  let domain = raw.trim();
  if (domain.endsWith(".")) domain = domain.slice(0, -1);
  if (!domain || domain.endsWith(".") || domain.startsWith(".")) return null;
  if (domain.startsWith("[")) {
    return /^\[[0-9a-fA-F:.]+\]$/.test(domain) ? domain.toLowerCase() : null;
  }
  if (NON_ASCII.test(domain)) {
    // The WHATWG URL host parser applies UTS #46 processing and yields the
    // A-label form. It is used for non-ASCII names only: for ASCII it would
    // also rewrite numeric forms to a dotted quad, which the server never does.
    try {
      domain = new URL(`http://${domain}/`).hostname;
    } catch {
      return null;
    }
    if (domain.endsWith(".")) domain = domain.slice(0, -1);
  }
  domain = domain.toLowerCase();
  if (domain.length > 253) return null;
  const labels = domain.split(".");
  return labels.every((label) => DNS_LABEL.test(label)) ? domain : null;
}

/** The server identity profile's classification of a canonical domain. */
export function domainKind(canonical: string): DomainKind {
  if (canonical.startsWith("[")) return "ipv6";
  if (IPV4_ADDRESS.test(canonical)) return "ipv4";
  const labels = canonical.split(".");
  if (NUMERIC_LABEL.test(labels[labels.length - 1] ?? "")) return "numeric-ipv4";
  if (labels.length === 1) return "single-label";
  return "dns";
}

/** Whether a domain of this kind can name a server on the federated network. */
function namesFederationPeer(canonical: string): boolean {
  return domainKind(canonical) === "dns";
}

/**
 * Canonical text of a bare JID with a localpart (`room@service`), or `null`.
 * A resource is refused, not dropped: a room is never addressed by an
 * occupant JID.
 */
export function canonicalRoomJid(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed || trimmed.includes("/")) return null;
  const at = trimmed.lastIndexOf("@");
  if (at <= 0) return null;
  const localpart = trimmed.slice(0, at);
  if (/[\s"&'/:<>@]/.test(localpart) || localpart.length > 1023) return null;
  const domain = canonicalDomain(trimmed.slice(at + 1));
  if (!domain) return null;
  return `${localpart.toLowerCase()}@${domain}`;
}

/**
 * XEP-0503 node IRI of a Space (`xmpp:service?;node=name`), the form rooms
 * name their parent with.
 */
export function spaceNodeIri(serviceJid: string, node: string): string {
  return `xmpp:${serviceJid}?;node=${encodeURIComponent(node)}`;
}

/**
 * The Space a node IRI names, or `null` when the value is no IRI that names
 * both a service and a node. A bare `node=…` value names no service and is
 * not a Space address.
 */
export function parseSpaceNodeIri(value: string | null | undefined): { serviceJid: string; node: string } | null {
  const uri = parseXmppUri(value ?? "");
  if (!uri || uri.path.includes("@")) return null;
  const node = uri.params.get("node");
  const serviceJid = canonicalDomain(uri.path);
  if (!node || !serviceJid) return null;
  return { serviceJid, node };
}

/**
 * Read a community address the user typed. Accepts `xmpp:` URIs
 * (`xmpp:room@service?join`, `xmpp:service?;node=name`), a bare room JID and
 * a service domain.
 */
export function parseCommunityAddress(input: string): ParsedCommunityAddress {
  const trimmed = input.trim();
  if (!trimmed) return { ok: false, error: "empty" };
  const uri = /^xmpp:/i.test(trimmed) ? parseXmppUri(trimmed) : null;
  if (/^xmpp:/i.test(trimmed) && !uri) return { ok: false, error: "malformed" };
  const path = uri ? uri.path : trimmed;
  const node = uri?.params.get("node") ?? null;
  if (path.includes("/")) return { ok: false, error: "resource" };

  if (path.includes("@")) {
    if (node) return { ok: false, error: "malformed" };
    const roomJid = canonicalRoomJid(path);
    if (!roomJid) return { ok: false, error: "malformed" };
    if (!namesFederationPeer(domainOf(roomJid))) return { ok: false, error: "refused-domain" };
    return { ok: true, address: { kind: "room", roomJid } };
  }

  const serviceJid = canonicalDomain(path);
  if (!serviceJid) return { ok: false, error: "malformed" };
  if (!namesFederationPeer(serviceJid)) return { ok: false, error: "refused-domain" };
  if (node) return { ok: true, address: { kind: "space", serviceJid, node } };
  return { ok: true, address: { kind: "service", serviceJid } };
}

/** The domain an address is hosted on. */
export function communityAddressDomain(address: CommunityAddress): string {
  return address.kind === "room" ? domainOf(address.roomJid) : address.serviceJid;
}

/**
 * Whether a domain belongs to the home deployment: its account domain, one
 * of the services it discovered, or a component its server lists. Everything
 * else is reached over server-to-server federation.
 */
export function isHomeDomain(domain: string, home: HomeAddresses): boolean {
  const canonical = canonicalDomain(domain);
  if (!canonical) return false;
  return [home.accountDomain, home.mucService, home.spacesService, ...(home.components ?? [])]
    .some((candidate) => !!candidate && canonicalDomain(domainOf(candidate)) === canonical);
}

/** Where a room JID is hosted; see {@link RoomHost}. */
export function roomHost(roomJid: string, home: HomeAddresses): RoomHost {
  const domain = domainOf(roomJid);
  if (!canonicalDomain(domain)) return "unknown";
  if (isHomeDomain(domain, home)) return "home";
  return home.mucService && canonicalDomain(domainOf(home.mucService)) ? "remote" : "unknown";
}

/**
 * Whether a room JID is known to be hosted by a server other than the home
 * deployment. False while the home MUC service is not discovered.
 */
export function isRemoteRoomJid(roomJid: string, home: HomeAddresses): boolean {
  return roomHost(roomJid, home) === "remote";
}

/** The canonical domain of a room's host, or `null` when the JID names none. */
export function roomHostDomain(roomJid: string): string | null {
  return canonicalDomain(domainOf(roomJid));
}

function domainOf(jid: string): string {
  const bare = jid.split("/")[0] ?? "";
  const at = bare.lastIndexOf("@");
  return at < 0 ? bare : bare.slice(at + 1);
}

/**
 * RFC 5122 `xmpp:` URI: an optional `//authority/` (ignored), the target JID
 * as path, and a query of an optional query type followed by `;key=value`
 * pairs. Percent escapes are decoded once.
 */
function parseXmppUri(value: string): { path: string; queryType: string | null; params: Map<string, string> } | null {
  const match = /^xmpp:(?:\/\/[^/]*\/)?([^?#]*)(?:\?([^#]*))?(?:#.*)?$/i.exec(value.trim());
  if (!match) return null;
  const path = decodeUriPart(match[1] ?? "");
  if (path === null || !path) return null;
  const params = new Map<string, string>();
  let queryType: string | null = null;
  const query = match[2];
  if (query !== undefined) {
    const [type = "", ...pairs] = query.split(";");
    queryType = type || null;
    for (const pair of pairs) {
      const separator = pair.indexOf("=");
      if (separator <= 0) continue;
      const key = decodeUriPart(pair.slice(0, separator));
      const pairValue = decodeUriPart(pair.slice(separator + 1));
      if (key === null || pairValue === null) return null;
      if (!params.has(key)) params.set(key, pairValue);
    }
  }
  return { path, queryType, params };
}

function decodeUriPart(value: string): string | null {
  try {
    return decodeURIComponent(value);
  } catch {
    return null;
  }
}
