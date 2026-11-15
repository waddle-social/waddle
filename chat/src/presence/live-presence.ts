// One presence per person, by bare JID, for every dot, label and grouping in
// the app. Three sources feed it: subscription presence (a contact's resources
// collapsed by `PresenceRegistry`), MUC occupant presence from every joined
// room whose occupant's real JID is disclosed (XEP-0045 §7.2.4), and this
// device's own chosen Show. Surfaces read it by JID instead of each deriving
// presence from whatever list they happen to hold, so they cannot disagree.

import { atom, computed, map } from "nanostores";
import { useStore } from "@nanostores/vue";
import { computed as vueComputed, type ComputedRef } from "vue";

import { bareJidKey } from "@/lib/xmpp/jid";
import type { OccupantPresence } from "@/lib/xmpp/types";
import { mostAvailableShow, type RenderedShow } from "./most-available";
import { $selfShow } from "./presence-store";

export type LivePresence = Readonly<Record<string, RenderedShow>>;

const $contactPresence = map<Record<string, RenderedShow>>({});
const $occupantPresence = atom<Record<string, OccupantPresence[]>>({});
const $selfJid = atom("");

/** The dot for a Show: extended away reads as away; unknown stays unknown. */
export function presenceFromShow(show: RenderedShow | undefined): OccupantPresence | undefined {
  switch (show) {
    case "available":
      return "online";
    case "away":
    case "xa":
      return "away";
    case "dnd":
      return "dnd";
    case "offline":
      return "offline";
    default:
      return undefined;
  }
}

/** Record a contact's rendered (resource-aggregated) subscription presence. */
export function setContactPresence(jid: string, show: RenderedShow): void {
  const key = bareJidKey(jid);
  if (key) $contactPresence.setKey(key, show);
}

/** Replace the occupant snapshot: every joined room's shows per real bare JID. */
export function setOccupantPresence(byJid: Record<string, OccupantPresence[]>): void {
  $occupantPresence.set(byJid);
}

/** The signed-in account, whose presence follows this device's chosen Show. */
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
    const merged: Record<string, RenderedShow> = {};
    // A room only hears the Show a client joined with: status changes go to
    // subscribers, not rooms. So a room says "connected", never "away", and it
    // fills in for people without subscription presence rather than override it.
    for (const [jid, shows] of Object.entries(occupants)) {
      merged[jid] = mostAvailableShow(shows.map((show) => (show === "online" ? "available" : show)));
    }
    Object.assign(merged, contacts);
    if (selfJid) merged[selfJid] = selfShow === "chat" ? "available" : selfShow;
    return merged;
  },
);

/** A person's Show in a snapshot, or `undefined` when nothing is known. */
export function liveShow(live: LivePresence, jid: string | null | undefined): RenderedShow | undefined {
  const key = bareJidKey(jid ?? "");
  return key ? live[key] : undefined;
}

/** Reactive Show for one JID; `undefined` when nothing is known about it. */
export function useLivePresence(jid: () => string | null | undefined): ComputedRef<RenderedShow | undefined> {
  const live = useStore($livePresence);
  return vueComputed(() => liveShow(live.value, jid()));
}
