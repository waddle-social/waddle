// The Spaces on other servers that the user chose to show.
//
// XEP-0402 bookmarks hold rooms; nothing standard holds the user's list of
// Spaces. This client keeps that list as XEP-0223 private data in a PEP
// node of the user's own account on the home server, one item per Space,
// so every device of the account sees the same list and two devices that
// change different Spaces never overwrite each other:
//
//   <item id='xmpp:spaces.example.org?;node=dev'>
//     <space xmlns='urn:waddle:joined-spaces:0'
//            service='spaces.example.org' node='dev' name='Developers' follow='true'
//            rev='0b8e…'/>
//   </item>
//
// The item id is the Space's node IRI in canonical form (service domain
// lower case, without a trailing dot), so a Space has exactly one item.
// The home server pins this node to its owner (whitelist access, no
// last-item delivery) whatever a client asks for, so the list is never
// readable by the user's contacts.
//
// `follow` records that the user wants the Space's updates, so the client
// renews its XEP-0060 subscription at sign-in; the host decides whether
// that subscription exists. `rev` is a random token each write of the
// entry sets anew, so a device can tell whether the entry changed since it
// last saw it without comparing clocks across devices. An entry is a preference only: it never stands
// for a membership, role or any permission. What the user may see or do in
// a Space and its rooms is answered by their hosts every time.
//
// Earlier versions kept the whole list as one XEP-0049 private XML
// document. The first load that finds such a document copies its entries
// into the PEP node and then clears it.

import { canonicalDomain, spaceNodeIri } from "./remote-address";
import { qualifiedSpaceKey } from "./qualified-identity";
import { stanzaErrorContext } from "./stanza-error-context";
import {
  buildIqXml,
  childElements,
  firstElementNS,
  parseXmlResponse,
  sendRawIq,
  submitField,
  type RawIqClient,
} from "./xml-stanza";

const NS_PRIVATE = "jabber:iq:private";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_DATAFORM = "jabber:x:data";
const PUBLISH_OPTIONS_FORM = "http://jabber.org/protocol/pubsub#publish-options";
/** Payload namespace and PEP node of the list. */
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
  /** Token of the write that stored this entry; absent on entries older clients wrote. */
  revision?: string;
}

/** Read and change the user's joined-space list, one Space at a time. */
export interface JoinedSpaceStore {
  load(): Promise<JoinedSpacePreference[]>;
  /** Write one Space's entry; other entries are untouched. */
  put(preference: JoinedSpacePreference): Promise<void>;
  /** Remove one Space's entry; other entries are untouched. */
  remove(preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): Promise<void>;
}

/** Canonical identity of an entry, or `""` when it names no Space. */
export function joinedSpaceKey(preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): string {
  const service = canonicalDomain(preference.serviceJid);
  return service ? qualifiedSpaceKey(service, preference.node) : "";
}

/** The PEP item id of an entry: the Space's canonical node IRI, or `""`. */
export function joinedSpaceItemId(preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): string {
  const service = canonicalDomain(preference.serviceJid);
  return service && preference.node ? spaceNodeIri(service, preference.node) : "";
}

/** One entry from a `<space/>` element, or `null` when it names no Space. */
function preferenceFromElement(entry: Element): JoinedSpacePreference | null {
  const serviceJid = canonicalDomain(entry.getAttribute("service") ?? "");
  const node = entry.getAttribute("node") ?? "";
  if (!serviceJid || !node) return null;
  const name = entry.getAttribute("name")?.trim();
  const follow = ["true", "1"].includes(entry.getAttribute("follow") ?? "");
  const revision = entry.getAttribute("rev")?.trim();
  return { serviceJid, node, ...(name ? { name } : {}), follow, ...(revision ? { revision } : {}) };
}

function deduplicated(entries: Iterable<JoinedSpacePreference | null>): JoinedSpacePreference[] {
  const seen = new Set<string>();
  const preferences: JoinedSpacePreference[] = [];
  for (const preference of entries) {
    if (!preference) continue;
    const key = joinedSpaceKey(preference);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    preferences.push(preference);
    if (preferences.length >= MAX_JOINED_SPACES) break;
  }
  return preferences;
}

/**
 * Entries of an XEP-0049 document written by earlier versions, in order,
 * without duplicates or entries that name no Space. Attributes outside the
 * schema are ignored.
 */
export function parseJoinedSpaces(doc: Document | Element): JoinedSpacePreference[] {
  const root = firstElementNS(doc, NS_JOINED_SPACES, "spaces");
  if (!root) return [];
  return deduplicated(childElements(root, "space", NS_JOINED_SPACES).map(preferenceFromElement));
}

/**
 * Entries of a PEP `<items/>` result. An item counts only when its id is
 * the canonical id of the Space its payload names, so a stray item can
 * never stand in for another Space.
 */
export function parseJoinedSpaceItems(doc: Document | Element): JoinedSpacePreference[] {
  const items = firstElementNS(doc, NS_PUBSUB, "items");
  if (!items) return [];
  return deduplicated(childElements(items, "item", NS_PUBSUB).map((item) => {
    const entry = childElements(item, "space", NS_JOINED_SPACES)[0];
    const preference = entry ? preferenceFromElement(entry) : null;
    return preference && item.getAttribute("id") === joinedSpaceItemId(preference) ? preference : null;
  }));
}

export function buildLoadJoinedSpacesIq(id: string): string {
  return buildIqXml({
    type: "get",
    id,
    payload: { name: "pubsub", ns: NS_PUBSUB, children: [{ name: "items", attrs: { node: NS_JOINED_SPACES } }] },
  });
}

/**
 * The publish request for one entry, with the XEP-0223 publish-options
 * (persistent, owner-only). The home server enforces the same itself.
 */
export function buildPutJoinedSpaceIq(id: string, preference: JoinedSpacePreference): string {
  const itemId = joinedSpaceItemId(preference);
  const serviceJid = canonicalDomain(preference.serviceJid);
  if (!itemId || !serviceJid) throw new Error("A joined-space entry needs a Space address.");
  return buildIqXml({
    type: "set",
    id,
    payload: {
      name: "pubsub",
      ns: NS_PUBSUB,
      children: [
        {
          name: "publish",
          attrs: { node: NS_JOINED_SPACES },
          children: [{
            name: "item",
            attrs: { id: itemId },
            children: [{
              name: "space",
              ns: NS_JOINED_SPACES,
              attrs: {
                service: serviceJid,
                node: preference.node,
                name: preference.name || undefined,
                follow: preference.follow ? "true" : undefined,
                rev: preference.revision || undefined,
              },
            }],
          }],
        },
        {
          name: "publish-options",
          children: [{
            name: "x",
            ns: NS_DATAFORM,
            attrs: { type: "submit" },
            children: [
              submitField("FORM_TYPE", PUBLISH_OPTIONS_FORM, "hidden"),
              submitField("pubsub#persist_items", "true"),
              submitField("pubsub#access_model", "whitelist"),
            ],
          }],
        },
      ],
    },
  });
}

/** The retract request for one entry, notifying the user's other clients. */
export function buildRemoveJoinedSpaceIq(id: string, preference: Pick<JoinedSpacePreference, "serviceJid" | "node">): string {
  const itemId = joinedSpaceItemId(preference);
  if (!itemId) throw new Error("A joined-space entry needs a Space address.");
  return buildIqXml({
    type: "set",
    id,
    payload: {
      name: "pubsub",
      ns: NS_PUBSUB,
      children: [{
        name: "retract",
        attrs: { node: NS_JOINED_SPACES, notify: "true" },
        children: [{ name: "item", attrs: { id: itemId } }],
      }],
    },
  });
}

/** The XEP-0049 request that reads the list earlier versions stored. */
export function buildLoadLegacyJoinedSpacesIq(id: string): string {
  return buildIqXml({
    type: "get",
    id,
    payload: { name: "query", ns: NS_PRIVATE, children: [{ name: "spaces", ns: NS_JOINED_SPACES }] },
  });
}

/** The XEP-0049 request that clears the list earlier versions stored. */
export function buildClearLegacyJoinedSpacesIq(id: string): string {
  return buildIqXml({
    type: "set",
    id,
    payload: { name: "query", ns: NS_PRIVATE, children: [{ name: "spaces", ns: NS_JOINED_SPACES }] },
  });
}

/** The store backed by the private PEP node of the user's own account. */
export function pepJoinedSpaceStore(xmpp: RawIqClient, ownBareJid: string): JoinedSpaceStore {
  async function loadItems(): Promise<JoinedSpacePreference[]> {
    const id = crypto.randomUUID();
    try {
      const response = await sendRawIq(xmpp, { xml: buildLoadJoinedSpacesIq(id), id, to: ownBareJid, node: NS_JOINED_SPACES });
      return parseJoinedSpaceItems(parseXmlResponse(response));
    } catch (error) {
      // No node yet: nothing is stored.
      if (stanzaErrorContext(error).condition === "item-not-found") return [];
      throw error;
    }
  }

  async function put(preference: JoinedSpacePreference): Promise<void> {
    const id = crypto.randomUUID();
    await sendRawIq(xmpp, { xml: buildPutJoinedSpaceIq(id, preference), id, to: ownBareJid, node: NS_JOINED_SPACES });
  }

  /**
   * Copy a list kept by earlier versions into the node, then clear it. The
   * old document is cleared only once every entry it held is in the node;
   * an entry the node already holds keeps the node's value.
   */
  async function migrateLegacy(current: JoinedSpacePreference[]): Promise<JoinedSpacePreference[]> {
    let legacy: JoinedSpacePreference[];
    try {
      const id = crypto.randomUUID();
      const response = await sendRawIq(xmpp, { xml: buildLoadLegacyJoinedSpacesIq(id), id, to: ownBareJid });
      legacy = parseJoinedSpaces(parseXmlResponse(response));
    } catch {
      // Nothing readable to move; the node is the list.
      return current;
    }
    if (legacy.length === 0) return current;
    const known = new Set(current.map(joinedSpaceKey));
    const missing = legacy.filter((preference) => !known.has(joinedSpaceKey(preference)));
    try {
      for (const preference of missing) await put(preference);
      const id = crypto.randomUUID();
      await sendRawIq(xmpp, { xml: buildClearLegacyJoinedSpacesIq(id), id, to: ownBareJid });
    } catch {
      // The old document stays and is moved again at the next load.
    }
    return [...current, ...missing];
  }

  return {
    async load() {
      return migrateLegacy(await loadItems());
    },
    put,
    async remove(preference) {
      const id = crypto.randomUUID();
      try {
        await sendRawIq(xmpp, { xml: buildRemoveJoinedSpaceIq(id, preference), id, to: ownBareJid, node: NS_JOINED_SPACES });
      } catch (error) {
        if (stanzaErrorContext(error).condition === "item-not-found") return;
        throw error;
      }
    },
  };
}

/**
 * A request to stop following a Space that its host, or the joined-space
 * list, has not confirmed yet. Kept on this device only, so it holds even
 * when the list itself could not be written.
 */
export interface SpaceUnfollowIntent {
  serviceJid: string;
  node: string;
  /** The host has ended the subscription, or retrying cannot change it. */
  remoteDone: boolean;
  /** The joined-space list no longer says to follow the Space. */
  preferenceDone: boolean;
  /**
   * The `revision` of the Space's list entry when this device last wrote or
   * read it for the unfollow; absent when the entry had none. An entry that
   * says to follow under another revision was written later, by a device
   * that followed again, and the unfollow no longer applies.
   */
  seenRevision?: string;
}

/**
 * Whether an unfollow was overtaken by a later follow written to the list
 * from another device (see `SpaceUnfollowIntent.seenRevision`).
 */
export function unfollowOvertaken(
  intent: SpaceUnfollowIntent,
  entry: JoinedSpacePreference | undefined,
): boolean {
  return !!entry && entry.follow && entry.revision !== intent.seenRevision;
}

export interface SpaceUnfollowIntentStore {
  load(): SpaceUnfollowIntent[];
  save(intents: readonly SpaceUnfollowIntent[]): void;
}

type KeyValueStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function browserStorage(): KeyValueStorage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/**
 * Unfollow requests kept in this browser's storage for one account. Storage
 * that is unavailable or full only loses the record: the unfollow itself
 * was already applied.
 */
export function localSpaceUnfollowIntentStore(
  ownBareJid: string,
  storage: KeyValueStorage | null = browserStorage(),
): SpaceUnfollowIntentStore {
  const storageKey = `waddle:space-unfollow-intents:${ownBareJid.toLowerCase()}`;
  return {
    load() {
      try {
        const raw = storage?.getItem(storageKey);
        if (!raw) return [];
        const parsed: unknown = JSON.parse(raw);
        if (!Array.isArray(parsed)) return [];
        return parsed.flatMap((value): SpaceUnfollowIntent[] => {
          if (typeof value !== "object" || value === null) return [];
          const entry = value as Record<string, unknown>;
          if (typeof entry.serviceJid !== "string" || typeof entry.node !== "string") return [];
          if (!joinedSpaceKey({ serviceJid: entry.serviceJid, node: entry.node })) return [];
          return [{
            serviceJid: entry.serviceJid,
            node: entry.node,
            remoteDone: entry.remoteDone === true,
            preferenceDone: entry.preferenceDone === true,
            ...(typeof entry.seenRevision === "string" && entry.seenRevision
              ? { seenRevision: entry.seenRevision }
              : {}),
          }];
        }).slice(0, MAX_JOINED_SPACES);
      } catch {
        return [];
      }
    },
    save(intents) {
      try {
        if (intents.length === 0) storage?.removeItem(storageKey);
        else storage?.setItem(storageKey, JSON.stringify(intents.slice(0, MAX_JOINED_SPACES)));
      } catch {
        // See above: losing the record does not undo the unfollow.
      }
    },
  };
}
