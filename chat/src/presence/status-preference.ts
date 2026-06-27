// Wire mapping + inbound half of cross-device manual-status sync (ADR-010
// Phase 4). The user's picked presence mode is persisted as a single PEP item
// on their own JID (`urn:waddle:status-preference:0`, XEP-0223). This module
// maps between the local `PresenceMode` and the wasm/wire form, and resolves an
// inbound pubsub event into the mode this device should adopt.
//
// Split into pure mappers plus a thin DOMParser shell (à la in-call-activity.ts)
// so the decisions are unit-tested without a real DOM.

import { barePeerJid } from "@/lib/xmpp/jid";

import type { ManualStatus, PresenceMode } from "./effective-show";

const NS_STATUS_PREFERENCE = "urn:waddle:status-preference:0";

/** The PEP node the preference is published to (equal to its namespace). */
const STATUS_PREFERENCE_PEP_NODE = NS_STATUS_PREFERENCE;

const MANUAL_STATUSES: readonly ManualStatus[] = ["available", "chat", "away", "dnd"];

function isManualStatus(value: string | null | undefined): value is ManualStatus {
  return value != null && (MANUAL_STATUSES as readonly string[]).includes(value);
}

/** The `{ mode, status? }` shape exchanged with the wasm bridge. */
export interface StatusPreferenceWire {
  mode: string;
  status?: string;
}

/**
 * Map a wire `{ mode, status }` to a `PresenceMode`, or `null` when it is not a
 * recognised shape (unknown mode, missing/invalid status on a manual pick).
 * Shared by the connect-time fetch and the live receive path.
 */
export function presenceModeFromWire(
  wire: StatusPreferenceWire | null | undefined,
): PresenceMode | null {
  if (!wire) return null;
  if (wire.mode === "automatic") return { kind: "automatic" };
  if (wire.mode === "manual" && isManualStatus(wire.status)) {
    return { kind: "manual", status: wire.status };
  }
  return null;
}

/** Map a `PresenceMode` to the wire form for publishing. */
export function presenceModeToWire(mode: PresenceMode): StatusPreferenceWire {
  return mode.kind === "manual"
    ? { mode: "manual", status: mode.status }
    : { mode: "automatic" };
}

/** One inbound pubsub item, structural subset of the wasm `WasmPubsubEventItem`. */
interface StatusPreferencePubsubItem {
  retracted?: boolean;
  payload: { kind: string; xml?: string };
}

/** One inbound pubsub event, structural subset of the wasm `WasmPubsubEvent`. */
interface StatusPreferencePubsubEvent {
  from?: string;
  node: string;
  items: StatusPreferencePubsubItem[];
}

/**
 * Resolve an inbound pubsub event into the `PresenceMode` this device should
 * adopt, or `null` when it carries no preference signal: a foreign node, no
 * sender, or a sender that is not our own bare JID. The node is owner-only
 * (whitelist), so in practice the only `from` we ever see is our own — but we
 * guard explicitly, since adopting another user's pick would be a takeover.
 *
 * The node is single-item (`id = current`); if several items arrive the last
 * wins. A retraction reads as Automatic — reset normally publishes an explicit
 * `automatic` item, but a stray retract must not strand a manual status.
 */
export function statusPreferenceUpdate(
  event: StatusPreferencePubsubEvent,
  selfBareJid: string,
): PresenceMode | null {
  if (event.node !== STATUS_PREFERENCE_PEP_NODE || !event.from) return null;
  if (barePeerJid(event.from) !== barePeerJid(selfBareJid)) return null;
  let update: PresenceMode | null = null;
  for (const item of event.items) {
    if (item.retracted) {
      update = { kind: "automatic" };
      continue;
    }
    if (item.payload.kind === "opaque" && item.payload.xml !== undefined) {
      update = parseStatusPreference(item.payload.xml);
    }
  }
  return update;
}

/** Parse incoming opaque `<status-preference/>` XML into a `PresenceMode`. */
export function parseStatusPreference(xml: string): PresenceMode | null {
  if (typeof DOMParser === "undefined") return null;
  const element = new DOMParser()
    .parseFromString(xml, "text/xml")
    .getElementsByTagNameNS(NS_STATUS_PREFERENCE, "status-preference")[0];
  if (!element) return null;
  return presenceModeFromWire({
    mode: element.getAttribute("mode") ?? "",
    status: element.getAttribute("status") ?? undefined,
  });
}
