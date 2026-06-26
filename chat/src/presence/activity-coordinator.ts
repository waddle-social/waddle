// Sole owner of the user's XEP-0108 activity PEP node (ADR-010 Phase 3). The
// node holds a single current activity item, so the automatic in-call overlay
// and the user's manual activity (Settings / feed composer) must not clobber
// each other: this publishes the *effective* activity — the in-call overlay
// while a call is active, otherwise the manual activity — and restores the
// manual activity (rather than blanket-retracting) when a call ends.
//
// Diff-driven so an unchanged effective state never re-publishes. Writes report
// whether they actually reached the wire; a write lost while disconnected leaves
// the baseline untouched so it retries on the next reconcile (a call-state /
// manual change, or session-ready) — no stale overlay can persist.

import { callOverlayActivity } from "@/lib/calls/in-call-overlay";
import type { CallState } from "@/lib/calls/types";
import type { ActivityPublication } from "@/lib/xmpp/pep-types";

function sameActivity(a: ActivityPublication | null, b: ActivityPublication | null): boolean {
  if (a === null || b === null) return a === b;
  return a.general === b.general && a.specific === b.specific && a.text === b.text;
}

/** What to write to the activity node before a graceful disconnect (logout). */
export type ActivityFlush =
  | { kind: "publish"; activity: ActivityPublication }
  | { kind: "retract" }
  | { kind: "none" };

/**
 * The write the activity node needs before a graceful disconnect. If a call is
 * active the wire holds the in-call overlay, so restore the user's manual
 * activity (or clear it) — no stale "in a call" survives on the server and the
 * chosen status is preserved across the logout. Otherwise the node already
 * reflects the manual activity, so leave it untouched. Pure, so the ghost-
 * cleanup-on-disconnect decision is unit-tested without the controller.
 */
export function activityFlushForDisconnect(
  callActive: boolean,
  manual: ActivityPublication | null,
): ActivityFlush {
  if (!callActive) return { kind: "none" };
  return manual ? { kind: "publish", activity: manual } : { kind: "retract" };
}

export interface ActivityCoordinatorDeps {
  /** Publish the activity item; returns whether it actually reached the wire. */
  publish: (activity: ActivityPublication) => boolean;
  /** Clear the activity item; returns whether it actually reached the wire. */
  retract: () => boolean;
  /** Current unified 1:1 + MUC call state. */
  callState: () => CallState;
  /** The user's manually-chosen activity, or null. */
  manualActivity: () => ActivityPublication | null;
}

export class ActivityCoordinator {
  private lastPublished: ActivityPublication | null = null;

  constructor(private readonly deps: ActivityCoordinatorDeps) {}

  /** The activity that should be on the wire: in-call overlay wins over manual. */
  private desired(): ActivityPublication | null {
    return callOverlayActivity(this.deps.callState()) ?? this.deps.manualActivity();
  }

  /** Reconcile the node to the effective activity. Idempotent; only writes on a
   *  real change, and only advances the baseline when the write was sent. */
  reconcile(): void {
    const desired = this.desired();
    if (sameActivity(desired, this.lastPublished)) return;
    const sent = desired === null ? this.deps.retract() : this.deps.publish(desired);
    if (sent) this.lastPublished = desired;
  }

  /** Forget the wire baseline (logout): the item belongs to the signed-out
   *  account, so the next session re-publishes from a clean baseline. */
  reset(): void {
    this.lastPublished = null;
  }
}
