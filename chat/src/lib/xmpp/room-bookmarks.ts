// XEP-0402 bookmarks for rooms, written by this client.
//
// A bookmark is the user's own record that a room exists for them: it is
// what a fresh device reads to list and rejoin rooms, local or remote, and
// its `autojoin` attribute says whether to join at login. It grants nothing
// in the room; the room's host decides every join.
//
// Editing republishes the item under the same id (the canonical room JID,
// which the server requires). XEP-0402 §3.4 requires keeping what this
// client does not understand, so a republish carries the stored `<nick/>`,
// `<password/>` and `<extensions/>` (where notification settings live)
// unchanged and only rewrites `name` and `autojoin`.

import { canonicalRoomJid } from "./remote-address";
import { stanzaErrorContext } from "./stanza-error-context";
import {
  buildIqXml,
  childElements,
  firstElementNS,
  parseXmlResponse,
  sendRawIq,
  submitField,
  type RawIqClient,
  type XmlElementSpec,
} from "./xml-stanza";

const NS_BOOKMARKS = "urn:xmpp:bookmarks:1";
const NS_PUBSUB = "http://jabber.org/protocol/pubsub";
const NS_DATAFORM = "jabber:x:data";
const PUBLISH_OPTIONS_FORM = "http://jabber.org/protocol/pubsub#publish-options";

export interface RoomBookmark {
  roomJid: string;
  name?: string;
  autojoin: boolean;
  /** The stored `<conference/>`, kept so a republish preserves its children. */
  stored?: Element;
}

/** The bookmark node exists with a configuration XEP-0402 does not allow. */
export class BookmarkNodeConfigError extends Error {
  readonly condition = "precondition-not-met";

  constructor() {
    super("Your bookmark storage is configured differently than expected, so the bookmark was not saved.");
    this.name = "BookmarkNodeConfigError";
  }
}

function requireRoomJid(roomJid: string): string {
  const canonical = canonicalRoomJid(roomJid);
  if (!canonical) throw new Error("A bookmark needs a room address.");
  return canonical;
}

/** Every item id the bookmark node holds for `roomJid`, in any spelling of its address. */
interface StoredRoomBookmarks {
  /** The bookmark to carry forward: the canonical item if there is one, else the first other spelling. */
  bookmark: RoomBookmark | null;
  /** Ids of items naming the same room under another spelling of its address. */
  otherSpellings: string[];
}

/**
 * Read the user's bookmarks of one room. The node is read whole, because
 * an item for the room may have been written by another client under a
 * spelling of its address that is not canonical (another letter case, a
 * trailing dot on the domain); every such item names the same room.
 */
async function fetchStoredRoomBookmarks(
  xmpp: RawIqClient,
  ownBareJid: string,
  roomJid: string,
): Promise<StoredRoomBookmarks> {
  const canonical = requireRoomJid(roomJid);
  const id = crypto.randomUUID();
  const xml = buildIqXml({
    type: "get",
    id,
    payload: { name: "pubsub", ns: NS_PUBSUB, children: [{ name: "items", attrs: { node: NS_BOOKMARKS } }] },
  });
  let response: string;
  try {
    response = await sendRawIq(xmpp, { xml, id, to: ownBareJid, node: NS_BOOKMARKS });
  } catch (error) {
    // No bookmark node yet: there is nothing stored.
    if (stanzaErrorContext(error).condition === "item-not-found") return { bookmark: null, otherSpellings: [] };
    throw error;
  }
  return findRoomBookmarks(parseXmlResponse(response), canonical);
}

/** The bookmarks of one room in an `<items/>` response, matched by canonical address. */
export function findRoomBookmarks(doc: Document | Element, roomJid: string): StoredRoomBookmarks {
  const canonical = canonicalRoomJid(roomJid);
  const items = firstElementNS(doc, NS_PUBSUB, "items");
  if (!canonical || !items) return { bookmark: null, otherSpellings: [] };
  let exact: RoomBookmark | null = null;
  let firstOther: RoomBookmark | null = null;
  const otherSpellings: string[] = [];
  for (const item of childElements(items, "item", NS_PUBSUB)) {
    const itemId = item.getAttribute("id") ?? "";
    if (canonicalRoomJid(itemId) !== canonical) continue;
    const bookmark = parseRoomBookmarkItem(doc, itemId);
    if (itemId === canonical) {
      exact = bookmark ? { ...bookmark, roomJid: canonical } : null;
    } else {
      otherSpellings.push(itemId);
      if (!firstOther && bookmark) firstOther = { ...bookmark, roomJid: canonical };
    }
  }
  return { bookmark: exact ?? firstOther, otherSpellings };
}

/** Read the user's bookmark of one room, in any spelling of its address, or `null`. */
export async function fetchRoomBookmark(
  xmpp: RawIqClient,
  ownBareJid: string,
  roomJid: string,
): Promise<RoomBookmark | null> {
  return (await fetchStoredRoomBookmarks(xmpp, ownBareJid, roomJid)).bookmark;
}

/** The bookmark for `itemId` in an `<items/>` response, or `null`. */
export function parseRoomBookmarkItem(doc: Document | Element, itemId: string): RoomBookmark | null {
  const items = firstElementNS(doc, NS_PUBSUB, "items");
  if (!items) return null;
  for (const item of childElements(items, "item", NS_PUBSUB)) {
    if (item.getAttribute("id") !== itemId) continue;
    const conference = childElements(item, "conference", NS_BOOKMARKS)[0];
    if (!conference) return null;
    const name = conference.getAttribute("name");
    return {
      roomJid: itemId,
      ...(name ? { name } : {}),
      autojoin: ["true", "1"].includes(conference.getAttribute("autojoin") ?? ""),
      stored: conference,
    };
  }
  return null;
}

/**
 * The publish request for a bookmark, with XEP-0402's publish-options. A
 * stored `<conference/>`'s children are moved into the new item unchanged.
 */
export function buildPublishRoomBookmarkIq(id: string, bookmark: RoomBookmark): string {
  const itemId = requireRoomJid(bookmark.roomJid);
  const preserved = bookmark.stored ? Array.from(bookmark.stored.children) : [];
  const conference: XmlElementSpec = {
    name: "conference",
    ns: NS_BOOKMARKS,
    // XEP-0402 defaults `autojoin` to false, so false is written by omission.
    attrs: {
      name: bookmark.name || undefined,
      autojoin: bookmark.autojoin ? "true" : undefined,
    },
    children: preserved,
  };
  return buildIqXml({
    type: "set",
    id,
    payload: {
      name: "pubsub",
      ns: NS_PUBSUB,
      children: [
        {
          name: "publish",
          attrs: { node: NS_BOOKMARKS },
          children: [{ name: "item", attrs: { id: itemId }, children: [conference] }],
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
              submitField("pubsub#max_items", "max"),
              submitField("pubsub#send_last_published_item", "never"),
              submitField("pubsub#access_model", "whitelist"),
            ],
          }],
        },
      ],
    },
  });
}

/**
 * Create or edit the user's bookmark of a room. `name` and `autojoin` are
 * written; everything else a stored bookmark holds is kept. A `name` of
 * `undefined` keeps the stored name.
 */
export async function saveRoomBookmark(
  xmpp: RawIqClient,
  ownBareJid: string,
  change: { roomJid: string; name?: string; autojoin: boolean },
): Promise<RoomBookmark> {
  const roomJid = requireRoomJid(change.roomJid);
  const stored = await fetchStoredRoomBookmarks(xmpp, ownBareJid, roomJid);
  return writeRoomBookmark(xmpp, ownBareJid, roomJid, change, stored);
}

/**
 * Turn `autojoin` off on the user's bookmark of a room, so no device joins
 * it at sign-in. A room without a bookmark is left without one: this never
 * adds a room to the user's list. Resolves whether a bookmark was written.
 */
export async function turnOffRoomBookmarkAutojoin(
  xmpp: RawIqClient,
  ownBareJid: string,
  roomJid: string,
): Promise<boolean> {
  const canonical = requireRoomJid(roomJid);
  const stored = await fetchStoredRoomBookmarks(xmpp, ownBareJid, canonical);
  // Another spelling of the address may still join at sign-in on a client
  // that reads it; writing the canonical item retracts those.
  if (!stored.bookmark || (!stored.bookmark.autojoin && stored.otherSpellings.length === 0)) return false;
  await writeRoomBookmark(xmpp, ownBareJid, canonical, { autojoin: false }, stored);
  return true;
}

async function writeRoomBookmark(
  xmpp: RawIqClient,
  ownBareJid: string,
  roomJid: string,
  change: { name?: string; autojoin: boolean },
  { bookmark: existing, otherSpellings }: StoredRoomBookmarks,
): Promise<RoomBookmark> {
  const name = change.name ?? existing?.name;
  const bookmark: RoomBookmark = {
    roomJid,
    ...(name ? { name } : {}),
    autojoin: change.autojoin,
    ...(existing?.stored ? { stored: existing.stored } : {}),
  };
  const id = crypto.randomUUID();
  try {
    await sendRawIq(xmpp, { xml: buildPublishRoomBookmarkIq(id, bookmark), id, to: ownBareJid, node: NS_BOOKMARKS });
  } catch (error) {
    if (stanzaErrorContext(error).condition === "precondition-not-met") throw new BookmarkNodeConfigError();
    throw error;
  }
  // The room now has its canonical item; other spellings would list it twice.
  for (const itemId of otherSpellings) await retractBookmarkItem(xmpp, ownBareJid, itemId);
  const { stored: _moved, ...saved } = bookmark;
  return saved;
}

/**
 * The retract request for one bookmark item, notifying the user's other
 * clients. `itemId` is the item's id as stored, which another client may
 * have spelled differently from the canonical room JID.
 */
export function buildRetractRoomBookmarkIq(id: string, itemId: string): string {
  if (!canonicalRoomJid(itemId)) throw new Error("A bookmark needs a room address.");
  return buildIqXml({
    type: "set",
    id,
    payload: {
      name: "pubsub",
      ns: NS_PUBSUB,
      children: [{
        name: "retract",
        attrs: { node: NS_BOOKMARKS, notify: "true" },
        children: [{ name: "item", attrs: { id: itemId } }],
      }],
    },
  });
}

async function retractBookmarkItem(xmpp: RawIqClient, ownBareJid: string, itemId: string): Promise<void> {
  const id = crypto.randomUUID();
  try {
    await sendRawIq(xmpp, { xml: buildRetractRoomBookmarkIq(id, itemId), id, to: ownBareJid, node: NS_BOOKMARKS });
  } catch (error) {
    if (stanzaErrorContext(error).condition === "item-not-found") return;
    throw error;
  }
}

/**
 * Remove the user's bookmark of a room (XEP-0402 §3.5), in every spelling
 * of its address the node holds. A bookmark that is already gone counts as
 * removed. This neither leaves the room nor changes anything the room's
 * host holds.
 */
export async function removeRoomBookmark(xmpp: RawIqClient, ownBareJid: string, roomJid: string): Promise<void> {
  const canonical = requireRoomJid(roomJid);
  const { otherSpellings } = await fetchStoredRoomBookmarks(xmpp, ownBareJid, canonical);
  await retractBookmarkItem(xmpp, ownBareJid, canonical);
  for (const itemId of otherSpellings) await retractBookmarkItem(xmpp, ownBareJid, itemId);
}
