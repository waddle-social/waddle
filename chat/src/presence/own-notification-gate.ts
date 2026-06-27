// Phase 5a (ADR-010): when the user's own Effective Show is Do Not Disturb,
// their client silences its own message banners + sounds. This is the
// presence-driven "Do Not Disturb" Show — distinct from, and composable with,
// the server-side `urn:waddle:dnd:0` quiet-hours schedule, which gates pushes
// on a timetable. Badge / unread counts are unaffected (a separate path).

import type { EffectiveShow } from "./effective-show";

/** Does the user's own Effective Show currently suppress their own banners/sounds? */
export function presenceSuppressesOwnNotifications(show: EffectiveShow): boolean {
  return show === "dnd";
}
