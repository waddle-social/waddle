// Typed model of the `urn:waddle:safety-scores:1` XEP-0422 fastening: the
// server's asynchronous per-message judgments (one community-enrichment
// signal plus six content-safety categories), visible to every
// participant. Measurement, not a moderation action.

/** Every judgment category this client understands, in display order.
 * Tokens are the server's `message_judgments.judgment_name` values. */
export const SAFETY_CATEGORIES = [
  "is_question",
  "safety:hate_speech",
  "safety:explicit",
  "safety:harassment",
  "safety:violence",
  "safety:self_harm",
  "safety:spam_scam",
] as const;

export type SafetyCategory = (typeof SAFETY_CATEGORIES)[number];

/** `is_question` is a community-enrichment signal; every other category
 * is a content-safety judgment. Only the latter can raise a warning. */
export function isSafetyCategory(category: SafetyCategory): boolean {
  return category !== "is_question";
}

export interface SafetyScore {
  category: SafetyCategory;
  /** Finite probability in `0..=1`. */
  probability: number;
  /** Version of the category's wording (`taxonomy-version`). */
  taxonomyVersion: string;
}

/** One judgment batch: every score came from one model call. */
export interface SafetyScores {
  modelVersion: string;
  /** Known categories only, at most one score per category. */
  scores: SafetyScore[];
}

/** XEP-0422 replace (§Replacing fastenings) or clear (§Removing
 * fastenings) of the safety-scores fastening on `targetId`. */
export type SafetyScoresFastening =
  | { targetId: string; kind: "replace"; scores: SafetyScores }
  | { targetId: string; kind: "clear" };
