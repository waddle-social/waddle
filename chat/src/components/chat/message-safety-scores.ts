import type { TimelineMessage } from "@/lib/chat-ui";
import { SAFETY_CATEGORIES, type SafetyCategory, type SafetyScore, type SafetyScores } from "@/lib/safety-scores/types";

// Presentation helpers for the per-message safety-scores disclosure. The
// scores are measurement/annotation visible to every participant, never a
// moderation verdict, so the copy stays neutral ("scores", not "flagged").

const CATEGORY_LABELS: Record<SafetyCategory, string> = {
  is_question: "Question",
  "safety:hate_speech": "Hate speech",
  "safety:explicit": "Explicit content",
  "safety:harassment": "Harassment",
  "safety:violence": "Violence",
  "safety:self_harm": "Self-harm",
};

/** Scores to surface on a row, or `null` when there is nothing to show
 * (no fastening, an empty batch, or a retracted message). */
export function visibleSafetyScores(
  message: Pick<TimelineMessage, "safetyScores" | "isRetracted">,
): SafetyScores | null {
  const scores = message.safetyScores;
  if (!scores || scores.scores.length === 0 || message.isRetracted) return null;
  return scores;
}

export function safetyCategoryLabel(category: SafetyCategory): string {
  return CATEGORY_LABELS[category];
}

/** Scores in canonical category order, independent of wire order. */
export function orderedSafetyScores(scores: SafetyScores): SafetyScore[] {
  return [...scores.scores].sort(
    (a, b) => SAFETY_CATEGORIES.indexOf(a.category) - SAFETY_CATEGORIES.indexOf(b.category),
  );
}

/** Whole-percent label; a non-zero probability below 0.5 % reads "<1%"
 * so it is never mistaken for an exact zero. */
export function formatSafetyProbability(probability: number): string {
  const percent = Math.round(probability * 100);
  if (percent === 0 && probability > 0) return "<1%";
  return `${percent}%`;
}

/** CSS width for the probability bar (clamped defensively). */
export function safetyProbabilityWidth(probability: number): string {
  return `${Math.min(100, Math.max(0, probability * 100))}%`;
}

export function safetyScoresToggleLabel(expanded: boolean): string {
  return expanded ? "Hide content scores" : "Show content scores";
}
