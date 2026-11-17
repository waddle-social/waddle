// The Spaces on other servers that the user chose to show.
//
// XEP-0402 bookmarks hold rooms; nothing standard holds the user's list of
// Spaces. This client keeps that list as a preference document in the
// user's XEP-0049 private XML storage on the home server, so every device
// of the account sees the same list:
//
//   <spaces xmlns='urn:waddle:joined-spaces:0'>
//     <space service='spaces.example.org' node='dev' name='Developers' follow='true'/>
//   </spaces>
//
// Each entry is keyed by the fully qualified Space address (service JID and
// node). `follow` records that the user wants the Space's updates, so the
// client renews its XEP-0060 subscription; the host decides whether that
// subscription exists. An entry is a preference only: it never stands for a
// membership, role or any permission. What the user may see or do in a
// Space and its rooms is answered by their hosts every time.
//
// Private XML storage is used rather than PEP because the server enforces
// a private access model only for PEP nodes it knows; a new PEP node would
// be readable by the user's contacts.

import { canonicalDomain } from "./remote-address";
import { qualifiedSpaceKey } from "./qualified-identity";
import {
  buildIqXml,
  childElements,
  firstElementNS,
  parseXmlResponse,
  sendRawIq,
  type RawIqClient,
} from "./xml-stanza";

const NS_PRIVATE = "jabber:iq:private";
const NS_JOINED_SPACES = "urn:waddle:joined-spaces:0";

/** Most entries kept; a longer list is cut, keeping the first entries. */
const MAX_JOINED_SPACES = 256;

export interface JoinedSpacePreference {
  serviceJid: string;
  node: string;
  /** The name the Space had when it was added, shown until its host answers. */
  name?: string;
  /** The user wants the Space's updates (an XEP-0060 subscription). */
  follow: boolean;
}

/** Read and replace the user's joined-space list. */
export interface JoinedSpaceStore {
  load(): Promise<JoinedSpacePreference[]>;
  save(preferences: readonly JoinedSpacePreference[]): Promise<void>;
}

/** Canonical identity of an entry, or `""` when it names no Space. */
export function joinedSpaceKey(preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): string {
  const service = canonicalDomain(preference.serviceJid);
  return service ? qualifiedSpaceKey(service, preference.node) : "";
}

/**
 * Entries of a stored document, in order, without duplicates or entries
 * that name no Space. Attributes outside the schema are ignored.
 */
export function parseJoinedSpaces(doc: Document | Element): JoinedSpacePreference[] {
  const root = firstElementNS(doc, NS_JOINED_SPACES, "spaces");
  if (!root) return [];
  const seen = new Set<string>();
  const preferences: JoinedSpacePreference[] = [];
  for (const entry of childElements(root, "space", NS_JOINED_SPACES)) {
    const serviceJid = canonicalDomain(entry.getAttribute("service") ?? "");
    const node = entry.getAttribute("node") ?? "";
    if (!serviceJid || !node) continue;
    const key = qualifiedSpaceKey(serviceJid, node);
    if (seen.has(key)) continue;
    seen.add(key);
    const name = entry.getAttribute("name")?.trim();
    const follow = ["true", "1"].includes(entry.getAttribute("follow") ?? "");
    preferences.push({ serviceJid, node, ...(name ? { name } : {}), follow });
    if (preferences.length >= MAX_JOINED_SPACES) break;
  }
  return preferences;
}

/** Add or update one entry, keyed by its qualified address. */
export function withJoinedSpace(
  preferences: readonly JoinedSpacePreference[],
  added: JoinedSpacePreference,
): JoinedSpacePreference[] {
  const key = joinedSpaceKey(added);
  if (!key) return [...preferences];
  const index = preferences.findIndex((preference) => joinedSpaceKey(preference) === key);
  if (index < 0) return [...preferences, added];
  return preferences.map((preference, position) => (position === index ? { ...preference, ...added } : preference));
}

/** Remove one entry, keyed by its qualified address. */
export function withoutJoinedSpace(
  preferences: readonly JoinedSpacePreference[],
  removed: Pick<JoinedSpacePreference, "serviceJid" | "node">,
): JoinedSpacePreference[] {
  const key = joinedSpaceKey(removed);
  return preferences.filter((preference) => joinedSpaceKey(preference) !== key);
}

export function buildLoadJoinedSpacesIq(id: string): string {
  return buildIqXml({
    type: "get",
    id,
    payload: {
      name: "query",
      ns: NS_PRIVATE,
      children: [{ name: "spaces", ns: NS_JOINED_SPACES }],
    },
  });
}

export function buildSaveJoinedSpacesIq(id: string, preferences: readonly JoinedSpacePreference[]): string {
  return buildIqXml({
    type: "set",
    id,
    payload: {
      name: "query",
      ns: NS_PRIVATE,
      children: [{
        name: "spaces",
        ns: NS_JOINED_SPACES,
        children: preferences.slice(0, MAX_JOINED_SPACES).map((preference) => ({
          name: "space",
          attrs: {
            service: preference.serviceJid,
            node: preference.node,
            name: preference.name || undefined,
            follow: preference.follow ? "true" : undefined,
          },
        })),
      }],
    },
  });
}

/** The store backed by the user's private XML storage on the home server. */
export function privateXmlJoinedSpaceStore(xmpp: RawIqClient, ownBareJid: string): JoinedSpaceStore {
  return {
    async load() {
      const id = crypto.randomUUID();
      const response = await sendRawIq(xmpp, { xml: buildLoadJoinedSpacesIq(id), id, to: ownBareJid });
      return parseJoinedSpaces(parseXmlResponse(response));
    },
    async save(preferences) {
      const id = crypto.randomUUID();
      await sendRawIq(xmpp, { xml: buildSaveJoinedSpacesIq(id, preferences), id, to: ownBareJid });
    },
  };
}
