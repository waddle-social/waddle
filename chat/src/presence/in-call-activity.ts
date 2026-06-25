// Inbound half of the in-call presence overlay (ADR-010 Phase 3). A contact's
// XEP-0108 User Activity arrives over PEP as opaque `<activity/>` XML; this
// reads whether it means "in a call" — `talking` with the `on_the_phone` or
// `on_video_phone` specific. Every other activity (or the empty retraction)
// reads as not-in-a-call, which clears the badge.
//
// Split into a pure mapper and a thin DOMParser shell (à la discovery.ts) so
// the decision is unit-tested without a DOM; bun:test has no DOMParser.

import {
  ACTIVITY_GENERAL_TALKING,
  ACTIVITY_SPECIFIC_ON_THE_PHONE,
  ACTIVITY_SPECIFIC_ON_VIDEO_PHONE,
} from "@/lib/xmpp/pep-types";

const NS_ACTIVITY = "http://jabber.org/protocol/activity";

/** The PEP node XEP-0108 activity is published to (equal to its namespace). */
export const ACTIVITY_PEP_NODE = NS_ACTIVITY;

/**
 * Whether a parsed `<activity/>` (its general + optional specific element
 * names) is an in-call overlay. Only `talking/on_the_phone` and
 * `talking/on_video_phone` count — a bare `talking`, `talking/in_real_life`,
 * or any other activity is not a call.
 */
export function inCallFromActivity(general: string, specific: string | null): boolean {
  if (general !== ACTIVITY_GENERAL_TALKING) return false;
  return specific === ACTIVITY_SPECIFIC_ON_THE_PHONE || specific === ACTIVITY_SPECIFIC_ON_VIDEO_PHONE;
}

/** Parse incoming opaque `<activity/>` XML into the in-call overlay boolean. */
export function parseActivityOverlay(xml: string): boolean {
  if (typeof DOMParser === "undefined") return false;
  const activity = new DOMParser()
    .parseFromString(xml, "text/xml")
    .getElementsByTagNameNS(NS_ACTIVITY, "activity")[0];
  if (!activity) return false;
  const general = Array.from(activity.children).find(
    (child) => child.namespaceURI === NS_ACTIVITY && child.localName !== "text",
  );
  if (!general) return false;
  const specific = Array.from(general.children).find((child) => child.namespaceURI === NS_ACTIVITY);
  return inCallFromActivity(general.localName, specific?.localName ?? null);
}
