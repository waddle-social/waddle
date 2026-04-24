/**
 * Typed protocol helpers for room/space creation.
 *
 * XEP-0045 (MUC) — standalone room creation/configuration.
 * XEP-0060 (PubSub) — Space node creation/configuration.
 * XEP-0402 / XEP-0503 — Bookmark publication into a Space node.
 *
 * Each helper accepts the raw stanza.js Agent so it can be tested in
 * isolation with lightweight mocks (no full BrowserXmppClient needed).
 */
import type { Agent } from "stanza";
import type { ReceivedMUCPresence } from "stanza/protocol";

// ── Namespaces ─────────────────────────────────────────────────────────────

const NS_MUC_ROOMCONFIG = "http://jabber.org/protocol/muc#roomconfig";
const NS_PUBSUB_NODE_CONFIG = "http://jabber.org/protocol/pubsub#node_config";
const NS_SPACES_0 = "urn:xmpp:spaces:0";
export const NS_BOOKMARKS_1 = "urn:xmpp:bookmarks:1";

// ── Parameter types ────────────────────────────────────────────────────────

interface CreateMucRoomParams {
  /** Local-part of the room JID (e.g. "general" → "general@muc.domain"). */
  roomLocalpart: string;
  /** Occupant nick used for the creation presence. */
  nick: string;
  name: string;
  description?: string;
  mucType?: "text" | "forum";
}

interface CreateSpaceNodeParams {
  /** Desired PubSub node id. If omitted the server assigns one. */
  nodeId?: string;
  name: string;
  description?: string;
}

interface PublishMucToSpaceParams {
  /** Human-readable conference name stored in the bookmark item. */
  name: string;
  autojoin?: boolean;
}

interface CreateMucInSpaceParams {
  roomLocalpart: string;
  nick: string;
  name: string;
  description?: string;
  mucType?: "text" | "forum";
  /** Existing Space node id to publish the new room into. */
  spaceNode: string;
}

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

// ── Result types ───────────────────────────────────────────────────────────

interface CreateMucRoomResult {
  roomJid: string;
}

interface CreateSpaceNodeResult {
  node: string;
  serviceJid: string;
}

interface CreateMucInSpaceResult {
  roomJid: string;
  spaceNode: string;
  spacesServiceJid: string;
}

interface CreateSpaceWithMucResult {
  roomJid: string;
  spaceNode: string;
  spacesServiceJid: string;
}

// ── Internal helpers ───────────────────────────────────────────────────────

function buildMucConfigFields(params: { name: string; description?: string; mucType?: "text" | "forum" }) {
  const fields: Array<{ name: string; value: string; type?: string }> = [
    { name: "FORM_TYPE", value: NS_MUC_ROOMCONFIG, type: "hidden" },
    { name: "muc#roomconfig_roomname", value: params.name, type: "text-single" },
    ...(params.description
      ? [{ name: "muc#roomconfig_roomdesc", value: params.description, type: "text-single" }]
      : []),
    ...(params.mucType === "forum"
      ? [{ name: "muc#roomconfig_forum", value: "1", type: "boolean" }]
      : []),
  ];
  return fields;
}

/**
 * Returns a Promise that resolves when the agent emits a `muc:available`
 * presence matching the expected full JID (room + "/" + nick), or rejects
 * after 15 s.
 */
function waitForRoomSelfPresence(
  xmpp: Agent,
  roomJid: string,
  nick: string,
): Promise<void> {
  const fullJid = `${roomJid}/${nick}`;

  return new Promise<void>((resolve, reject) => {
    const timeout = setTimeout(() => {
      xmpp.off("muc:available", onPresence as (...args: unknown[]) => void);
      reject(new Error(`Timed out waiting for room self-presence: ${roomJid}`));
    }, 15_000);

    const onPresence = (pres: ReceivedMUCPresence) => {
      if ((pres.from ?? "") === fullJid || String(pres.from ?? "").startsWith(`${fullJid}`)) {
        clearTimeout(timeout);
        xmpp.off("muc:available", onPresence as (...args: unknown[]) => void);
        resolve();
      }
    };

    xmpp.on("muc:available", onPresence as (...args: unknown[]) => void);
  });
}

// ── XEP-0045: MUC room creation ────────────────────────────────────────────

/**
 * Create and configure a MUC room via XEP-0045.
 *
 * 1. Join the room (creates it; server assigns owner affiliation and emits
 *    self-presence, typically with status code 201).
 * 2. Send owner configuration IQ with a roomconfig data form.
 * 3. Leave the room — BrowserXmppClient's switchRoom manages re-join.
 */
export async function createMucRoom(
  xmpp: Agent,
  mucServiceJid: string,
  params: CreateMucRoomParams,
): Promise<CreateMucRoomResult> {
  const roomJid = `${params.roomLocalpart}@${mucServiceJid}`;

  const selfPresenceReady = waitForRoomSelfPresence(xmpp, roomJid, params.nick);

  await xmpp.joinRoom(roomJid, params.nick, { history: { maxStanzas: 0 } } as Parameters<typeof xmpp.joinRoom>[2]);
  await selfPresenceReady;

  await xmpp.sendIQ({
    type: "set",
    to: roomJid,
    muc: {
      type: "configure",
      form: {
        type: "submit",
        fields: buildMucConfigFields(params),
      },
    },
  } as Parameters<Agent["sendIQ"]>[0]);

  try {
    await xmpp.leaveRoom(roomJid, params.nick);
  } catch {
    // best-effort leave; BrowserXmppClient will manage re-join via switchRoom
  }

  return { roomJid };
}

// ── XEP-0060: PubSub Space node creation ──────────────────────────────────

/**
 * Create and configure a PubSub node on the Space service (XEP-0060) with
 * `pubsub#type = urn:xmpp:spaces:0` to mark it as a Waddle Space (XEP-0503).
 */
export async function createSpaceNode(
  xmpp: Agent,
  spacesServiceJid: string,
  params: CreateSpaceNodeParams,
): Promise<CreateSpaceNodeResult> {
  const nodeId = params.nodeId ?? `space-${Date.now()}`;

  const configFields: Array<{ name: string; value: string; type?: string }> = [
    { name: "FORM_TYPE", value: NS_PUBSUB_NODE_CONFIG, type: "hidden" },
    { name: "pubsub#type", value: NS_SPACES_0, type: "text-single" },
    { name: "pubsub#title", value: params.name, type: "text-single" },
    { name: "pubsub#access_model", value: "open", type: "list-single" },
    ...(params.description
      ? [{ name: "pubsub#description", value: params.description, type: "text-single" }]
      : []),
  ];

  await xmpp.sendIQ({
    type: "set",
    to: spacesServiceJid,
    pubsub: {
      create: { node: nodeId },
      configure: {
        node: nodeId,
        form: {
          type: "submit",
          fields: configFields,
        },
      },
    },
  } as Parameters<Agent["sendIQ"]>[0]);

  return { node: nodeId, serviceJid: spacesServiceJid };
}

// ── XEP-0402 / XEP-0503: Bookmark publication into a Space ────────────────

/**
 * Publish a XEP-0402 `<conference>` bookmark item into an existing Space
 * node (XEP-0503), linking the MUC into the Space.
 *
 * The item id is the bare MUC JID.
 */
export async function publishMucToSpace(
  xmpp: Agent,
  spacesServiceJid: string,
  spaceNode: string,
  mucJid: string,
  params: PublishMucToSpaceParams,
): Promise<void> {
  await xmpp.sendIQ({
    type: "set",
    to: spacesServiceJid,
    pubsub: {
      publish: {
        node: spaceNode,
        item: {
          id: mucJid,
          content: {
            itemType: NS_BOOKMARKS_1,
            name: params.name,
            autojoin: params.autojoin ?? true,
          },
        },
      },
    },
  } as Parameters<Agent["sendIQ"]>[0]);
}

// ── Composed: MUC in an existing Space ────────────────────────────────────

/**
 * Create a MUC room then publish it as a bookmark into an existing Space node.
 *
 * Order:
 * 1. createMucRoom (XEP-0045)
 * 2. publishMucToSpace (XEP-0402/XEP-0503)
 */
export async function createMucInSpace(
  xmpp: Agent,
  mucServiceJid: string,
  spacesServiceJid: string,
  params: CreateMucInSpaceParams,
): Promise<CreateMucInSpaceResult> {
  const { roomJid } = await createMucRoom(xmpp, mucServiceJid, {
    roomLocalpart: params.roomLocalpart,
    nick: params.nick,
    name: params.name,
    description: params.description,
    mucType: params.mucType,
  });

  await publishMucToSpace(xmpp, spacesServiceJid, params.spaceNode, roomJid, {
    name: params.name,
    autojoin: true,
  });

  return { roomJid, spaceNode: params.spaceNode, spacesServiceJid };
}

// ── Composed: new Space together with its first MUC ───────────────────────

/**
 * Create a Space node then create a MUC and publish it into the new Space.
 *
 * Order:
 * 1. createSpaceNode (XEP-0060)
 * 2. createMucRoom (XEP-0045)
 * 3. publishMucToSpace (XEP-0402/XEP-0503)
 */
export async function createSpaceWithMuc(
  xmpp: Agent,
  mucServiceJid: string,
  spacesServiceJid: string,
  params: CreateSpaceWithMucParams,
): Promise<CreateSpaceWithMucResult> {
  const { node: spaceNode } = await createSpaceNode(xmpp, spacesServiceJid, {
    nodeId: params.spaceNodeId,
    name: params.spaceName,
    description: params.spaceDescription,
  });

  const { roomJid } = await createMucRoom(xmpp, mucServiceJid, {
    roomLocalpart: params.roomLocalpart,
    nick: params.nick,
    name: params.mucName,
    description: params.mucDescription,
    mucType: params.mucType,
  });

  await publishMucToSpace(xmpp, spacesServiceJid, spaceNode, roomJid, {
    name: params.mucName,
    autojoin: true,
  });

  return { roomJid, spaceNode, spacesServiceJid };
}
