// Orchestrates the pinned-panel body-cache lifecycle.
//
// - On panel open: hydratePinnedBodiesOnPanelOpen fetches every
//   pinned stanza-id not already represented in the room's loaded
//   channel timeline. Single batched MAM IQ per panel-open via the
//   XEP-0359 §3 stanza-id filter.
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
  const timelineIds = new Set(args.timelineMessages.map((m) => m.id));
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
  const cached = archived
    .map((m) => {
      const message = args.convert(m);
      return message ? { stanzaId: m.id ?? m.mam_id, message } : null;
    })
    .filter((m): m is { stanzaId: string; message: TimelineMessage } => m !== null);
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
  if (args.timelineMessages.some((m) => m.id === args.stanzaId)) return;
  const room = $pinnedMessageBodies.get().get(args.roomJid);
  if (room?.has(args.stanzaId)) return;

  const epoch = pinnedMessageBodiesEpoch();
  const archived = await args.client.fetchRoomMessagesByStanzaIds(
    args.spaceId,
    args.channelId,
    [args.stanzaId],
  );
  const cached = archived
    .map((m) => {
      const message = args.convert(m);
      return message ? { stanzaId: m.id ?? m.mam_id, message } : null;
    })
    .filter((m): m is { stanzaId: string; message: TimelineMessage } => m !== null);
  cachePinnedMessageBodies(args.roomJid, cached, epoch);
}
