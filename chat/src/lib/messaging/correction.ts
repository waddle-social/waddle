import type { TimelineMessage } from "@/lib/chat-ui";
import { findMessageIndexById } from "@/lib/message-ids";
import type { MergeableMessage } from "@/lib/messaging/types";

// XEP-0308 last-message-correction merge mechanics shared by the channel
// and DM pipelines. The divergent part is the sender-identity check
// (divergence 3) — MUC occupant JID (+ optional real-JID compare) on the
// channel side vs bare `fromJid` on the DM side — supplied as a
// `CorrectionPolicy` at the call site.

/** The replacement content carried by a XEP-0308 correction stanza. */
export type CorrectionPayload = Pick<
  MergeableMessage,
  | "body"
  | "markup"
  | "references"
  | "linkPreviews"
  | "extensionAnnotations"
  | "extensionBodyFallback"
>;

export interface CorrectionPolicy {
  /** Divergence 3: sender identity key (occupant JID vs bare JID). */
  senderMatches: (target: TimelineMessage) => boolean;
}

/**
 * Writes the correction payload onto `target` (in place). Markup /
 * references / link previews / extension annotations are replaced
 * wholesale to mirror the wire shape — null/empty drops the field.
 * The body is taken verbatim; live callers trim it (`applyCorrection`),
 * MAM replay does not.
 */
export function assignCorrectionFields(
  target: TimelineMessage,
  payload: CorrectionPayload,
): void {
  target.body = payload.body;
  target.isEdited = true;
  if (payload.markup && payload.markup.length > 0) target.markup = payload.markup;
  else delete target.markup;
  if (payload.references && payload.references.length > 0) target.references = payload.references;
  else delete target.references;
  if (payload.linkPreviews && payload.linkPreviews.length > 0) target.linkPreviews = payload.linkPreviews;
  else delete target.linkPreviews;
  if (payload.extensionAnnotations && payload.extensionAnnotations.length > 0) {
    target.extensionAnnotations = payload.extensionAnnotations;
    if (payload.extensionBodyFallback) target.extensionBodyFallback = true;
    else delete target.extensionBodyFallback;
  } else {
    delete target.extensionAnnotations;
    delete target.extensionBodyFallback;
  }
}

/**
 * Applies a live XEP-0308 correction to the timeline. Returns the updated
 * timeline, or `null` on target miss / sender mismatch so callers can
 * skip the ref reassignment (a spoofed correction must not rewrite
 * someone else's message).
 */
export function applyCorrection(
  messages: TimelineMessage[],
  replacesId: string,
  payload: CorrectionPayload,
  policy: CorrectionPolicy,
): TimelineMessage[] | null {
  const index = findMessageIndexById(messages, replacesId);
  if (index < 0) return null;
  const current = messages[index]!;
  if (!policy.senderMatches(current)) return null;
  const updated: TimelineMessage = { ...current };
  assignCorrectionFields(updated, { ...payload, body: payload.body.trim() });
  const next = messages.slice();
  next[index] = updated;
  return next;
}
