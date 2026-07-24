import type {
  DiscoveredChannel,
  RoomCatalogFingerprintField,
} from "./types";
import { bareJidKey } from "./jid";

export const ROOM_CATALOG_FINGERPRINT_FIELDS = [
  "spaceId",
  "autojoin",
  "isGroupDm",
  "isBookmarked",
] as const satisfies readonly RoomCatalogFingerprintField[];

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
  /**
   * Fields proven by a partial first observation. Missing means every
   * fingerprint field is authoritative.
   */
  catalogFingerprintFields?: RoomCatalogFingerprintField[];
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
  options: {
    absentRoomKeysAuthoritative?: boolean;
    authoritativeFingerprintFields?: ReadonlyMap<
      string,
      ReadonlySet<RoomCatalogFingerprintField>
    >;
  } = {},
): {
  blocks: Map<string, RoomAutoJoinBlock>;
  unblockedRoomKeys: string[];
  changed: boolean;
} {
  const catalog = new Map<string, string>();
  const roomsByKey = new Map<string, DiscoveredChannel>();
  for (const room of rooms) {
    const roomJid = room.jid ? bareJidKey(room.jid) : "";
    if (!roomJid) continue;
    catalog.set(roomJid, roomCatalogFingerprint(room));
    roomsByKey.set(roomJid, room);
  }
  const blocks = new Map<string, RoomAutoJoinBlock>();
  const unblockedRoomKeys: string[] = [];
  let changed = false;

  for (const [key, block] of current) {
    const roomIsPresent = catalog.has(key);
    const authoritativeFields = roomIsPresent
      ? options.authoritativeFingerprintFields?.get(key)
        ?? (options.authoritativeFingerprintFields
          ? undefined
          : options.absentRoomKeysAuthoritative !== false
            ? new Set(ROOM_CATALOG_FINGERPRINT_FIELDS)
            : undefined)
      : undefined;
    const fingerprintIsAuthoritative = roomIsPresent
      ? !!authoritativeFields?.size
      : options.absentRoomKeysAuthoritative !== false;
    if (!fingerprintIsAuthoritative) {
      blocks.set(key, block);
      continue;
    }
    const currentFingerprint = roomIsPresent ? catalog.get(key)! : null;
    if (block.catalogFingerprint === undefined) {
      const observedFields = roomIsPresent
        ? orderedRoomCatalogFingerprintFields(authoritativeFields!)
        : [];
      blocks.set(key, {
        ...block,
        catalogFingerprint: currentFingerprint,
        ...(roomIsPresent
            && !hasCompleteRoomCatalogFingerprintAuthority(authoritativeFields!)
          ? { catalogFingerprintFields: observedFields }
          : {}),
      });
      changed = true;
      continue;
    }
    const baselineFields = roomIsPresent
      ? new Set(
        block.catalogFingerprintFields
          ?? ROOM_CATALOG_FINGERPRINT_FIELDS,
      )
      : undefined;
    const comparableFields = roomIsPresent
      ? intersectRoomCatalogFingerprintFields(
          authoritativeFields!,
          baselineFields!,
        )
      : undefined;
    if (
      roomIsPresent
        ? roomCatalogFingerprintChanged(
            block.catalogFingerprint,
            roomsByKey.get(key)!,
            comparableFields!,
          )
        : block.catalogFingerprint !== currentFingerprint
    ) {
      unblockedRoomKeys.push(key);
      changed = true;
      continue;
    }
    if (roomIsPresent && block.catalogFingerprintFields) {
      const expandedFields = new Set([
        ...baselineFields!,
        ...authoritativeFields!,
      ]);
      if (expandedFields.size > baselineFields!.size) {
        const expandedFingerprint = mergeRoomCatalogFingerprint(
          block.catalogFingerprint,
          roomsByKey.get(key)!,
          authoritativeFields!,
        );
        if (expandedFingerprint) {
          const {
            catalogFingerprintFields: _discardPartialFields,
            ...blockWithoutPartialFields
          } = block;
          blocks.set(
            key,
            hasCompleteRoomCatalogFingerprintAuthority(expandedFields)
              ? {
                ...blockWithoutPartialFields,
                catalogFingerprint: expandedFingerprint,
              }
              : {
                ...block,
                catalogFingerprint: expandedFingerprint,
                catalogFingerprintFields:
                  orderedRoomCatalogFingerprintFields(expandedFields),
              },
          );
          changed = true;
          continue;
        }
      }
    }
    blocks.set(key, block);
  }

  return { blocks, unblockedRoomKeys, changed };
}

export function roomCatalogFingerprint(room: DiscoveredChannel): string {
  return JSON.stringify(roomCatalogFingerprintData(room));
}

export function hasCompleteRoomCatalogFingerprintAuthority(
  fields: ReadonlySet<RoomCatalogFingerprintField>,
): boolean {
  return ROOM_CATALOG_FINGERPRINT_FIELDS.every((field) => fields.has(field));
}

function orderedRoomCatalogFingerprintFields(
  fields: ReadonlySet<RoomCatalogFingerprintField>,
): RoomCatalogFingerprintField[] {
  return ROOM_CATALOG_FINGERPRINT_FIELDS.filter((field) => fields.has(field));
}

function intersectRoomCatalogFingerprintFields(
  left: ReadonlySet<RoomCatalogFingerprintField>,
  right: ReadonlySet<RoomCatalogFingerprintField>,
): Set<RoomCatalogFingerprintField> {
  return new Set(
    ROOM_CATALOG_FINGERPRINT_FIELDS.filter(
      (field) => left.has(field) && right.has(field),
    ),
  );
}

function mergeRoomCatalogFingerprint(
  baseline: string | null,
  room: DiscoveredChannel,
  fields: ReadonlySet<RoomCatalogFingerprintField>,
): string | null {
  if (baseline === null) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(baseline);
  } catch {
    return null;
  }
  if (!isRoomCatalogFingerprintData(parsed)) return null;
  const current = roomCatalogFingerprintData(room);
  for (const field of fields) parsed[field] = current[field];
  return JSON.stringify(parsed);
}

function roomCatalogFingerprintChanged(
  baseline: string | null,
  room: DiscoveredChannel,
  fields: ReadonlySet<RoomCatalogFingerprintField>,
): boolean {
  if (baseline === null) return true;
  let parsed: unknown;
  try {
    parsed = JSON.parse(baseline);
  } catch {
    return hasCompleteRoomCatalogFingerprintAuthority(fields)
      && baseline !== roomCatalogFingerprint(room);
  }
  if (!isRoomCatalogFingerprintData(parsed)) {
    return hasCompleteRoomCatalogFingerprintAuthority(fields)
      && baseline !== roomCatalogFingerprint(room);
  }
  const current = roomCatalogFingerprintData(room);
  return [...fields].some(
    (field) => !Object.is(parsed[field], current[field]),
  );
}

type RoomCatalogFingerprintData = {
  roomKey: string;
  roomId: string;
} & Record<RoomCatalogFingerprintField, string | boolean | null>;

function roomCatalogFingerprintData(
  room: DiscoveredChannel,
): RoomCatalogFingerprintData {
  return {
    roomKey: room.jid ? bareJidKey(room.jid) : "",
    roomId: room.id,
    spaceId: room.spaceId ?? null,
    autojoin: room.autojoin ?? null,
    isGroupDm: room.isGroupDm ?? false,
    isBookmarked: room.isBookmarked ?? false,
  };
}

function isRoomCatalogFingerprintData(
  value: unknown,
): value is RoomCatalogFingerprintData {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<RoomCatalogFingerprintData>;
  return typeof candidate.roomKey === "string"
    && typeof candidate.roomId === "string"
    && (typeof candidate.spaceId === "string" || candidate.spaceId === null)
    && (typeof candidate.autojoin === "boolean" || candidate.autojoin === null)
    && typeof candidate.isGroupDm === "boolean"
    && typeof candidate.isBookmarked === "boolean";
}
