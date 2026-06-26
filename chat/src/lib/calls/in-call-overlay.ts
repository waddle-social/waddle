// The in-call half of the XEP-0108 presence overlay (ADR-010 Phase 3): the pure
// mapping from call state to the activity to publish. Joining a 1:1 or MUC call
// is `talking/on_the_phone` (audio) or `talking/on_video_phone` (video); the
// overlay is orthogonal to the Show — it never changes Available/Away/DND. The
// `ActivityCoordinator` owns the node and merges this with the user's manual
// activity (a call overrides it, then it is restored on leave).

import {
  ACTIVITY_GENERAL_TALKING,
  ACTIVITY_SPECIFIC_ON_THE_PHONE,
  ACTIVITY_SPECIFIC_ON_VIDEO_PHONE,
  type ActivityPublication,
} from "@/lib/xmpp/pep-types";
import type { CallState } from "./types";

/**
 * The XEP-0108 activity for a given call state, or `null` when the call implies
 * no overlay (any non-active phase). A live call is `talking`; the specific
 * sub-activity carries the audio/video distinction.
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
