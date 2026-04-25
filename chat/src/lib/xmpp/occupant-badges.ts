import type { MemberSummary } from "@/lib/chat-types";
import type { OccupantHat, RoomHats } from "@/lib/xmpp-client";

const MUC_ROLE_HATS = {
  owner: { uri: "urn:xmpp:hats:owner", title: "Owner" },
  admin: { uri: "urn:xmpp:hats:admin", title: "Admin" },
  moderator: { uri: "urn:xmpp:hats:moderator", title: "Moderator" },
} as const satisfies Record<string, OccupantHat>;

type BadgeRole = keyof typeof MUC_ROLE_HATS;

function normalizeRoleHat(value: string | null | undefined): BadgeRole | null {
  if (value === "owner" || value === "admin" || value === "moderator") return value;
  return null;
}

function moderatorRoleForAffiliation(affiliation: string | null | undefined): "moderator" | null {
  return affiliation === "owner" || affiliation === "admin" ? "moderator" : null;
}

export function roleHatsForOccupant(
  affiliation?: string | null,
  role?: string | null,
): OccupantHat[] {
  const hats: OccupantHat[] = [];
  const affiliationHat = normalizeRoleHat(affiliation);
  if (affiliationHat) hats.push(MUC_ROLE_HATS[affiliationHat]);
  const roleHat = normalizeRoleHat(role ?? moderatorRoleForAffiliation(affiliation));
  if (roleHat) hats.push(MUC_ROLE_HATS[roleHat]);
  return hats;
}

export function mergeOccupantHats(...groups: Array<readonly OccupantHat[] | undefined>): OccupantHat[] {
  const merged: OccupantHat[] = [];
  const seen = new Set<string>();
  for (const group of groups) {
    for (const hat of group ?? []) {
      if (!hat.uri || !hat.title || seen.has(hat.uri)) continue;
      seen.add(hat.uri);
      merged.push(hat);
    }
  }
  return merged;
}

export function roomHatsFromMembers(members: readonly MemberSummary[]): RoomHats {
  const hats: RoomHats = {};
  for (const member of members) {
    const memberHats = roleHatsForOccupant(member.role);
    if (memberHats.length > 0) hats[member.username] = memberHats;
  }
  return hats;
}

export function mergeRoomHats(...sources: Array<Readonly<RoomHats> | undefined>): RoomHats {
  const merged: RoomHats = {};
  for (const source of sources) {
    if (!source) continue;
    for (const [nick, hats] of Object.entries(source)) {
      const next = mergeOccupantHats(merged[nick], hats);
      if (next.length > 0) merged[nick] = next;
    }
  }
  return merged;
}
