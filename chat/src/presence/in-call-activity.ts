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
import { barePeerJid } from "@/lib/xmpp/jid";

const NS_ACTIVITY = "http://jabber.org/protocol/activity";

/** The PEP node XEP-0108 activity is published to (equal to its namespace). */
const ACTIVITY_PEP_NODE = NS_ACTIVITY;

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

/**
 * One inbound pubsub item, structurally typed to the subset this decision
 * reads (assignable from the wasm `WasmPubsubEventItem`).
 */
interface ActivityPubsubItem {
  retracted?: boolean;
  payload: { kind: string; xml?: string };
}

/** One inbound pubsub event, structural subset of the wasm `WasmPubsubEvent`. */
interface ActivityPubsubEvent {
  from?: string;
  node: string;
  items: ActivityPubsubItem[];
}

/**
 * Resolve an inbound pubsub event into a single in-call overlay update for the
 * publishing contact, or `null` when it carries no overlay signal: a foreign
 * node, no sender, our own self-echo (XEP-0163 §3.4 notifies our other
 * resources with our bare JID — never badge ourselves), or no items. The
 * activity node is single-item (id `current`); if several items arrive the last
 * wins, matching what the node would hold.
 */
export function activityOverlayUpdate(
  event: ActivityPubsubEvent,
  selfBareJid: string,
): { jid: string; inCall: boolean } | null {
  if (event.node !== ACTIVITY_PEP_NODE || !event.from) return null;
  if (barePeerJid(event.from) === barePeerJid(selfBareJid)) return null;
  let update: { jid: string; inCall: boolean } | null = null;
  for (const item of event.items) {
    const inCall =
      !item.retracted && item.payload.kind === "opaque" && item.payload.xml !== undefined
        ? parseActivityOverlay(item.payload.xml)
        : false;
    update = { jid: event.from, inCall };
  }
  return update;
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
