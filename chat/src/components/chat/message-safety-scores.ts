import type { TimelineMessage } from "@/lib/chat-ui";
import {
  isSafetyCategory,
  SAFETY_CATEGORIES,
  type SafetyCategory,
  type SafetyScore,
  type SafetyScores,
} from "@/lib/safety-scores/types";

// Presentation helpers for the per-message safety-scores disclosure. The
// scores are measurement/annotation visible to every participant, never a
// moderation verdict, so the copy stays neutral ("scores", not "flagged").
//
// The chip is quiet by design: it appears only once a content-safety
// category is at least `SAFETY_NOTICE_THRESHOLD` (amber), and turns red at
// `SAFETY_ALERT_THRESHOLD`. The `is_question` signal never raises it, and
// the expanded breakdown lists only the categories at or above the notice
// threshold, so a reader sees what tripped the chip and nothing else.

const CATEGORY_LABELS: Record<SafetyCategory, string> = {
  is_question: "Question",
  "safety:hate_speech": "Hate speech",
  "safety:explicit": "Explicit content",
  "safety:harassment": "Harassment",
  "safety:violence": "Violence",
  "safety:self_harm": "Self-harm",
  "safety:spam_scam": "Spam or scam",
};

/** Probability at which a category is worth surfacing (amber). */
const SAFETY_NOTICE_THRESHOLD = 0.5;
/** Probability at which a category is alarming (red). */
const SAFETY_ALERT_THRESHOLD = 0.8;

export type SafetySeverity = "notice" | "alert";

/** Severity of one probability, or `null` below the notice threshold. */
export function safetyProbabilitySeverity(probability: number): SafetySeverity | null {
  if (probability >= SAFETY_ALERT_THRESHOLD) return "alert";
  if (probability >= SAFETY_NOTICE_THRESHOLD) return "notice";
  return null;
}

/** The chip's severity: the highest content-safety probability in the
 * batch, ignoring `is_question`. `null` means nothing to show. */
export function safetyScoresSeverity(scores: SafetyScores): SafetySeverity | null {
  let highest: number | null = null;
  for (const score of scores.scores) {
    if (!isSafetyCategory(score.category)) continue;
    if (highest === null || score.probability > highest) highest = score.probability;
  }
  return highest === null ? null : safetyProbabilitySeverity(highest);
}

/** Scores to surface on a row, or `null` when there is nothing to show
 * (no fastening, an empty batch, a retracted message, or no content-safety
 * category at the notice threshold). */
export function visibleSafetyScores(
  message: Pick<TimelineMessage, "safetyScores" | "isRetracted">,
): SafetyScores | null {
  const scores = message.safetyScores;
  if (!scores || scores.scores.length === 0 || message.isRetracted) return null;
  return safetyScoresSeverity(scores) === null ? null : scores;
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

/** The breakdown's rows: every category at or above the notice threshold,
 * in canonical order. Anything below it is hidden. */
export function notableSafetyScores(scores: SafetyScores): SafetyScore[] {
  return orderedSafetyScores(scores).filter(
    (score) => safetyProbabilitySeverity(score.probability) !== null,
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

/** Text colour for a severity: amber for a notice, red for an alert. */
export function safetySeverityTextClass(severity: SafetySeverity): string {
  return severity === "alert" ? "text-red-600 dark:text-red-400" : "text-amber-600 dark:text-amber-400";
}

/** Bar fill for a row's own severity; muted below the notice threshold. */
export function safetyProbabilityBarClass(probability: number): string {
  switch (safetyProbabilitySeverity(probability)) {
    case "alert":
      return "bg-red-500/80";
    case "notice":
      return "bg-amber-500/80";
    default:
      return "bg-muted-foreground/45";
  }
}

export function safetyScoresToggleLabel(expanded: boolean, severity: SafetySeverity): string {
  const noun = severity === "alert" ? "content alerts" : "content notices";
  return expanded ? `Hide ${noun}` : `Show ${noun}`;
}
