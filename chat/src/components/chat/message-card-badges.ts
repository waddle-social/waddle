import type { MucAffiliation, MucRole, OccupantAuthority, OccupantHat } from "@/lib/xmpp-client";

// Two separate badge layers:
//
// 1. **Authority badge** — owner / admin / moderator. Derived from the
//    `authority` prop, which mirrors the XEP-0045 `<x muc#user>` payload
//    on each presence. Server-enforced; clients render it but don't
//    invent it.
//
// 2. **Descriptive badge** — bot / verified / future descriptive hats.
//    Derived from XEP-0317 `<hats>`. Client renders; no protocol
//    semantics.
//
// At most one badge is shown next to the author name to keep the meta
// row breathable. Authority outranks descriptive: if you're an admin
// and a bot, the badge says ADMIN. Profile drawer surfaces show the
// full hat list separately.
export interface BadgeView {
  label: string;
  /** Tailwind class for the chip's text colour. */
  colorClass: string;
  /** Stable sort key used to pick the senior badge when multiple
   * candidates exist (e.g., admin + bot). Higher wins. */
  rank: number;
}

export function authorityBadge(authority: OccupantAuthority | null | undefined): BadgeView | null {
  const affiliation: MucAffiliation = authority?.affiliation ?? "none";
  const role: MucRole = authority?.role ?? "none";
  if (affiliation === "owner") {
    return { label: "OWNER", colorClass: "text-warning/70", rank: 4 };
  }
  if (affiliation === "admin") {
    return { label: "ADMIN", colorClass: "text-primary/75", rank: 3 };
  }
  // Role=Moderator without owner/admin affiliation is the "promoted
  // for this session" case — still authoritative, still rendered.
  if (role === "moderator") {
    return { label: "MOD", colorClass: "text-primary/75", rank: 2 };
  }
  return null;
}

// Waddle's descriptive hat URIs live under `urn:waddle:hats:*`, not
// `urn:xmpp:hats:*`. XEP-0317 deliberately leaves the hat URI value
// space open; minting Waddle-specific semantics inside the XSF
// reserve would falsely claim XSF registration. The container
// namespace `urn:xmpp:hats:0` is unchanged because that one is
// spec-defined.
const DESCRIPTIVE_HAT_LABELS: Record<string, string> = {
  "urn:waddle:hats:bot": "BOT",
  "urn:waddle:hats:verified": "VERIFIED",
};

const DESCRIPTIVE_HAT_COLORS: Record<string, string> = {
  "urn:waddle:hats:bot": "text-success/75",
  "urn:waddle:hats:verified": "text-primary/75",
};

const DESCRIPTIVE_HAT_RANK: Record<string, number> = {
  "urn:waddle:hats:verified": 1,
  "urn:waddle:hats:bot": 0,
};

export function descriptiveBadge(hats: OccupantHat[] | null | undefined): BadgeView | null {
  if (!hats || hats.length === 0) return null;
  let best: BadgeView | null = null;
  for (const hat of hats) {
    const label = DESCRIPTIVE_HAT_LABELS[hat.uri] ?? hat.title;
    const colorClass = DESCRIPTIVE_HAT_COLORS[hat.uri] ?? "text-muted-foreground";
    const rank = DESCRIPTIVE_HAT_RANK[hat.uri] ?? 0;
    if (best === null || rank > best.rank) {
      best = { label, colorClass, rank };
    }
  }
  return best;
}

/**
 * Single chip rendered next to the author name. Authority outranks
 * descriptive hats so e.g. an admin who is also a bot shows ADMIN —
 * authority is the load-bearing fact about that occupant in the room;
 * the bot tag is supplementary and visible elsewhere (profile drawer).
 */
export function authorBadge(
  authority: OccupantAuthority | null | undefined,
  hats: OccupantHat[] | null | undefined,
): BadgeView | null {
  const fromAuthority = authorityBadge(authority);
  const fromHats = descriptiveBadge(hats);
  if (fromAuthority && fromHats) {
    return fromAuthority.rank >= fromHats.rank ? fromAuthority : fromHats;
  }
  return fromAuthority ?? fromHats;
}

/**
 * Tooltip on the chip lists every layer the occupant carries so a
 * user hovering "ADMIN" still discovers the bot/verified tags.
 */
export function authorBadgeTooltip(
  authority: OccupantAuthority | null | undefined,
  hats: OccupantHat[] | null | undefined,
): string {
  const labels: string[] = [];
  const fromAuthority = authorityBadge(authority);
  if (fromAuthority) labels.push(fromAuthority.label);
  for (const hat of hats ?? []) {
    labels.push(DESCRIPTIVE_HAT_LABELS[hat.uri] ?? hat.title);
  }
  return labels.join(" · ");
}
