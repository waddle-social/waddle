// Typed model of the `urn:waddle:safety-scores:1` XEP-0422 fastening: the
// server's asynchronous per-message judgments (one community-enrichment
// signal plus five content-safety categories), visible to every
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
] as const;

export type SafetyCategory = (typeof SAFETY_CATEGORIES)[number];

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

/** Room-authored XEP-0422 result bound to one original stanza and revision. */
export interface SafetyScoresFastening {
  targetOriginId: string;
  targetStanzaId: string;
  targetStanzaBy: string;
  sourceRevisionId: string;
  scores: SafetyScores;
}
