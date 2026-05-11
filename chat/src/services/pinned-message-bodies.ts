// Orchestrates the pinned-panel body-cache lifecycle.
//
// - On panel open: hydratePinnedBodiesOnPanelOpen fetches every
//   pinned stanza-id not already represented in the room's loaded
//   channel timeline. Single batched MAM IQ per panel-open via the
//   Waddle-specific MAM stanza-id filter (custom data-form var per
//   XEP-0313 §4.2 + XEP-0068; field var: {urn:waddle:mam-stanza-id:0}stanza-id).
// - On `applyPinEvent("pinned")`: hydrateSinglePinnedBody fetches the
//   new entry's body (skipping the round-trip if the message is
//   already on screen).
// - Eviction on unpin lives in the `pinned-messages` store's update
//   pipeline; this module does not own that path.
//
// `convert` is a required argument because the canonical
// WasmArchivedMessage → TimelineMessage path runs through
// `roomMessageFromArchived` (WasmArchivedMessage → LiveRoomMessage)
// followed by `mapLiveRoomMessageToTimeline` (LiveRoomMessage →
// TimelineMessage), and the latter requires a WaddleSession.
// Task 14 will supply the bound converter when wiring this service
// into the channel controller.

import {
  $pinnedMessageBodies,
  cachePinnedMessageBodies,
  pinnedMessageBodiesEpoch,
} from "@/stores/pinned-message-bodies";
import { $pinnedRooms } from "@/stores/pinned-messages";
import type { TimelineMessage } from "@/lib/chat-ui";
import type { WasmArchivedMessage } from "@/lib/xmpp/wasm-types";

/**
 * Match an archived message against a set of REQUESTED stanza-ids, preferring
 * the room-scoped XEP-0359 id (the canonical UUID pins reference). Returns the
 * matched requested id so that the cache key equals what the caller asked for.
 */
function matchRequestedStanzaId(
  archived: WasmArchivedMessage,
  roomJid: string,
  requested: Set<string>,
): string | null {
  // Room-stamped XEP-0359 id (the canonical UUID pin uses).
  const roomStamped = archived.stanza_ids?.find((s) => s.by === roomJid)?.id;
  if (roomStamped && requested.has(roomStamped)) return roomStamped;
  // Fallback: singular stanza_id field scoped to the room.
  if (
    archived.stanza_id &&
    archived.stanza_id_by === roomJid &&
    requested.has(archived.stanza_id)
  ) {
    return archived.stanza_id;
  }
  // Last resort: the wire message id (covers legacy clients that use the
  // canonical UUID as the wire id attribute).
  if (archived.id && requested.has(archived.id)) return archived.id;
  return null;
}

/**
 * Build a set of all ids by which a timeline message can be identified:
 * m.id, m.reactionTargetId, m.replyableId, and all entries in m.wireIds.
 * Used to short-circuit MAM fetches for messages already on screen.
 */
function timelinePresenceSet(messages: ReadonlyArray<TimelineMessage>): Set<string> {
  const set = new Set<string>();
  for (const m of messages) {
    set.add(m.id);
    if (m.reactionTargetId) set.add(m.reactionTargetId);
    if (m.replyableId) set.add(m.replyableId);
    for (const wid of m.wireIds ?? []) set.add(wid);
  }
  return set;
}

interface MamFetcher {
  fetchRoomMessagesByStanzaIds: (
    spaceId: string,
    channelId: string,
    stanzaIds: string[],
  ) => Promise<WasmArchivedMessage[]>;
}

interface HydrateOpenArgs {
  client: MamFetcher;
  spaceId: string;
  channelId: string;
  roomJid: string;
  timelineMessages: ReadonlyArray<TimelineMessage>;
  /** Converter WasmArchivedMessage → TimelineMessage. Required because
   * the canonical path needs a WaddleSession (for `isSelf`) that this
   * module does not own. Task 14 supplies the bound converter. */
  convert: (archived: WasmArchivedMessage) => TimelineMessage | null;
}

export async function hydratePinnedBodiesOnPanelOpen(args: HydrateOpenArgs): Promise<void> {
  const room = $pinnedRooms.get().get(args.roomJid);
  if (!room) return;
  const cache = $pinnedMessageBodies.get().get(args.roomJid) ?? new Map();
  const timelineIds = timelinePresenceSet(args.timelineMessages);
  const missing = room.entries
    .map((e) => e.target_stanza_id)
    .filter((id) => !timelineIds.has(id) && !cache.has(id));
  if (missing.length === 0) return;

  const epoch = pinnedMessageBodiesEpoch();
  const archived = await args.client.fetchRoomMessagesByStanzaIds(
    args.spaceId,
    args.channelId,
    missing,
  );
  const requestedSet = new Set(missing);
  const cached = archived.flatMap((m) => {
    const stanzaId = matchRequestedStanzaId(m, args.roomJid, requestedSet);
    if (!stanzaId) return [];
    const message = args.convert(m);
    return message ? [{ stanzaId, message }] : [];
  });
  cachePinnedMessageBodies(args.roomJid, cached, epoch);
}

interface HydrateSingleArgs {
  client: MamFetcher;
  spaceId: string;
  channelId: string;
  roomJid: string;
  stanzaId: string;
  timelineMessages: ReadonlyArray<TimelineMessage>;
  /** Converter WasmArchivedMessage → TimelineMessage. Required; see
   * HydrateOpenArgs.convert. */
  convert: (archived: WasmArchivedMessage) => TimelineMessage | null;
}

export async function hydrateSinglePinnedBody(args: HydrateSingleArgs): Promise<void> {
  const timelineIds = timelinePresenceSet(args.timelineMessages);
  if (timelineIds.has(args.stanzaId)) return;
  const room = $pinnedMessageBodies.get().get(args.roomJid);
  if (room?.has(args.stanzaId)) return;

  const epoch = pinnedMessageBodiesEpoch();
  const archived = await args.client.fetchRoomMessagesByStanzaIds(
    args.spaceId,
    args.channelId,
    [args.stanzaId],
  );
  const requestedSet = new Set([args.stanzaId]);
  const cached = archived.flatMap((m) => {
    const stanzaId = matchRequestedStanzaId(m, args.roomJid, requestedSet);
    if (!stanzaId) return [];
    const message = args.convert(m);
    return message ? [{ stanzaId, message }] : [];
  });
  cachePinnedMessageBodies(args.roomJid, cached, epoch);
}
