// One presence dot per person, by bare JID, for any avatar in the app.
// Three sources feed it: subscription presence (a contact's resources
// collapsed by `PresenceRegistry`), MUC occupant presence from every joined
// room whose occupant's real JID is disclosed (XEP-0045 §7.2.4), and this
// device's own chosen Show. `UserAvatar` reads it by JID, so a surface no
// longer needs to thread presence through props for the dot to appear.

import { atom, computed, map } from "nanostores";
import { useStore } from "@nanostores/vue";
import { computed as vueComputed, type ComputedRef } from "vue";

import { bareJidKey } from "@/lib/xmpp/jid";
import type { OccupantPresence } from "@/lib/xmpp/types";
import type { RenderedShow } from "./most-available";
import { $selfShow } from "./presence-store";

type LivePresence = Record<string, OccupantPresence>;

const $contactPresence = map<LivePresence>({});
const $occupantPresence = atom<Record<string, OccupantPresence[]>>({});
const $selfJid = atom("");

// Mirrors `mostAvailableShow`: a deliberate Do Not Disturb wins, then
// available, then away; offline is the floor.
const ORDER: OccupantPresence[] = ["dnd", "online", "away", "offline"];

function fromShow(show: RenderedShow | "chat"): OccupantPresence {
  if (show === "available" || show === "chat") return "online";
  if (show === "xa") return "away";
  return show;
}

/** Record a contact's rendered (resource-aggregated) subscription presence. */
export function setContactPresence(jid: string, show: RenderedShow): void {
  const key = bareJidKey(jid);
  if (key) $contactPresence.setKey(key, fromShow(show));
}

/** Replace the occupant snapshot: every joined room's shows per real bare JID. */
export function setOccupantPresence(byJid: Record<string, OccupantPresence[]>): void {
  $occupantPresence.set(byJid);
}

/** The signed-in account, whose dot follows this device's chosen Show. */
export function setSelfPresenceJid(jid: string | null | undefined): void {
  $selfJid.set(bareJidKey(jid ?? ""));
}

/** Forget subscription presence. Call when the presence registry is cleared. */
export function resetContactPresence(): void {
  $contactPresence.set({});
}

export const $livePresence = computed(
  [$contactPresence, $occupantPresence, $selfJid, $selfShow],
  (contacts, occupants, selfJid, selfShow): LivePresence => {
    const merged: LivePresence = {};
    for (const key of new Set([...Object.keys(contacts), ...Object.keys(occupants)])) {
      const shows = [contacts[key], ...(occupants[key] ?? [])];
      const best = ORDER.find((show) => shows.includes(show));
      if (best) merged[key] = best;
    }
    if (selfJid) merged[selfJid] = fromShow(selfShow);
    return merged;
  },
);

/** Reactive presence for one JID; `undefined` when nothing is known about it. */
export function useLivePresence(jid: () => string | null | undefined): ComputedRef<OccupantPresence | undefined> {
  const live = useStore($livePresence);
  return vueComputed(() => {
    const key = bareJidKey(jid() ?? "");
    return key ? live.value[key] : undefined;
  });
}
