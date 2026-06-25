// Outbound half of the in-call presence overlay (ADR-010 Phase 3). Joining a
// 1:1 or MUC call publishes XEP-0108 User Activity `talking/on_the_phone`
// (audio) or `talking/on_video_phone` (video) over PEP so a contact's roster /
// DM clients can layer an "in a call" badge on top of their existing Show.
// Leaving retracts it. The overlay is orthogonal to the Show — it never
// changes Available/Away/DND.

import {
  ACTIVITY_GENERAL_TALKING,
  ACTIVITY_SPECIFIC_ON_THE_PHONE,
  ACTIVITY_SPECIFIC_ON_VIDEO_PHONE,
  type ActivityPublication,
} from "@/lib/xmpp/pep-types";
import type { CallState } from "./types";

/**
 * The XEP-0108 activity to publish for a given call state, or `null` when no
 * overlay should be on the wire (any non-active phase ⇒ retract). A live call
 * is `talking`; the specific sub-activity carries the audio/video distinction.
 */
export function callOverlayActivity(state: CallState): ActivityPublication | null {
  if (state.phase !== "active") return null;
  return {
    general: ACTIVITY_GENERAL_TALKING,
    specific: state.media.video
      ? ACTIVITY_SPECIFIC_ON_VIDEO_PHONE
      : ACTIVITY_SPECIFIC_ON_THE_PHONE,
  };
}

function sameActivity(a: ActivityPublication | null, b: ActivityPublication | null): boolean {
  if (a === null || b === null) return a === b;
  return a.general === b.general && a.specific === b.specific;
}

export interface CallOverlayPublisherDeps {
  /** Publish the in-call XEP-0108 activity item over PEP. */
  publish: (activity: ActivityPublication) => void;
  /** Clear the activity item (publish the empty `<activity/>` retraction). */
  retract: () => void;
}

/**
 * Drives the outbound overlay from call-state transitions. Fed each new
 * `CallState`, it diffs the desired overlay against what it last put on the
 * wire and emits a publish or retract only on a real change — so a stream of
 * identical "active" states never re-publishes, and toggling video mid-call
 * re-publishes the matching specific. It never retracts something it never
 * published (idle→idle is silent). One instance owns the activity node, so the
 * wire and `lastPublished` stay in lock-step.
 */
export class CallOverlayPublisher {
  private lastPublished: ActivityPublication | null = null;

  constructor(private readonly deps: CallOverlayPublisherDeps) {}

  update(state: CallState): void {
    const desired = callOverlayActivity(state);
    if (sameActivity(desired, this.lastPublished)) return;
    this.lastPublished = desired;
    if (desired === null) {
      this.deps.retract();
    } else {
      this.deps.publish(desired);
    }
  }
}
