import type { DiscoveredChannel } from "./types";
import { bareJidKey } from "./jid";

export type TerminalMucJoinCondition =
  | "registration-required"
  | "forbidden";

export type RoomAutoJoinBlock = {
  roomJid: string;
  condition: TerminalMucJoinCondition;
  /**
   * Stable room-catalog fingerprint captured when access was denied.
   * Missing means topology had not been discovered yet; `null` means the
   * room was absent from the discovered catalog.
   */
  catalogFingerprint?: string | null;
};

export function terminalMucJoinCondition(
  errorType: string | undefined,
  condition: string | undefined,
): TerminalMucJoinCondition | null {
  if (errorType !== "auth") return null;
  return condition === "registration-required" || condition === "forbidden"
    ? condition
    : null;
}

export function reconcileAutoJoinBlocks(
  current: ReadonlyMap<string, RoomAutoJoinBlock>,
  rooms: readonly DiscoveredChannel[],
): {
  blocks: Map<string, RoomAutoJoinBlock>;
  unblockedRoomKeys: string[];
  changed: boolean;
} {
  const catalog = new Map<string, string>();
  const bookmarkedRoomKeys = new Set<string>();
  for (const room of rooms) {
    const roomJid = room.jid ? bareJidKey(room.jid) : "";
    if (!roomJid) continue;
    catalog.set(roomJid, roomCatalogFingerprint(room));
    if (room.isBookmarked) bookmarkedRoomKeys.add(roomJid);
  }
  const blocks = new Map<string, RoomAutoJoinBlock>();
  const unblockedRoomKeys: string[] = [];
  let changed = false;

  for (const [key, block] of current) {
    const currentFingerprint = catalog.get(key) ?? null;
    if (block.catalogFingerprint === undefined) {
      if (bookmarkedRoomKeys.has(key)) {
        unblockedRoomKeys.push(key);
        changed = true;
        continue;
      }
      blocks.set(key, { ...block, catalogFingerprint: currentFingerprint });
      changed = true;
      continue;
    }
    if (block.catalogFingerprint !== currentFingerprint) {
      unblockedRoomKeys.push(key);
      changed = true;
      continue;
    }
    blocks.set(key, block);
  }

  return { blocks, unblockedRoomKeys, changed };
}

export function roomCatalogFingerprint(room: DiscoveredChannel): string {
  return JSON.stringify([
    room.jid ? bareJidKey(room.jid) : "",
    room.id,
    room.spaceId ?? null,
    room.autojoin ?? null,
    room.isGroupDm ?? false,
    room.isBookmarked ?? false,
  ]);
}
