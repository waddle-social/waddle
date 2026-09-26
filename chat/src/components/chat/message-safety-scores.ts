import {
  CircleHelp,
  EyeOff,
  HeartPulse,
  Inbox,
  MessageSquareWarning,
  ShieldAlert,
  UserRoundX,
  Zap,
} from "lucide-vue-next";
import type { Component } from "vue";
import type { TimelineMessage } from "@/lib/chat-ui";
import {
  isSafetyCategory,
  SAFETY_CATEGORIES,
  type SafetyCategory,
  type SafetyScore,
  type SafetyScores,
} from "@/lib/safety-scores/types";

// The scores annotate a message; they are not a moderation verdict. Colour
// and icon identify the category, while probability controls visibility and
// the length of its bar. light-dark() follows the app's color-scheme setting.
// Question is an enrichment signal, not a warning.
interface CategoryPresentation {
  label: string;
  icon: Component;
  textClass: string;
  barClass: string;
}

const CATEGORY_PRESENTATION = {
  is_question: {
    label: "Question", icon: CircleHelp,
    textClass: "text-[light-dark(#2563EB,#60A5FA)]",
    barClass: "bg-[light-dark(#2563EB,#60A5FA)]",
  },
  "safety:hate_speech": {
    label: "Hate speech", icon: UserRoundX,
    textClass: "text-[light-dark(#DC2626,#F87171)]",
    barClass: "bg-[light-dark(#DC2626,#F87171)]",
  },
  "safety:explicit": {
    label: "Explicit content", icon: EyeOff,
    textClass: "text-[light-dark(#7C3AED,#A78BFA)]",
    barClass: "bg-[light-dark(#7C3AED,#A78BFA)]",
  },
  "safety:harassment": {
    label: "Harassment", icon: MessageSquareWarning,
    textClass: "text-[light-dark(#C2410C,#FB923C)]",
    barClass: "bg-[light-dark(#C2410C,#FB923C)]",
  },
  "safety:violence": {
    label: "Violence", icon: Zap,
    textClass: "text-[light-dark(#BE123C,#FB7185)]",
    barClass: "bg-[light-dark(#BE123C,#FB7185)]",
  },
  "safety:self_harm": {
    label: "Self-harm", icon: HeartPulse,
    textClass: "text-[light-dark(#0F766E,#2DD4BF)]",
    barClass: "bg-[light-dark(#0F766E,#2DD4BF)]",
  },
  "safety:spam": {
    label: "Spam", icon: Inbox,
    textClass: "text-[light-dark(#475569,#CBD5E1)]",
    barClass: "bg-[light-dark(#475569,#CBD5E1)]",
  },
  "safety:scam": {
    label: "Scam", icon: ShieldAlert,
    textClass: "text-[light-dark(#B45309,#FBBF24)]",
    barClass: "bg-[light-dark(#B45309,#FBBF24)]",
  },
} satisfies Record<SafetyCategory, CategoryPresentation>;

/** Probability at which a category is worth surfacing. */
const SAFETY_NOTICE_THRESHOLD = 0.5;
/** Probability at which a category is an alert. */
const SAFETY_ALERT_THRESHOLD = 0.8;
/** Probability at which a question signal gets its own message marker. */
export const QUESTION_MARKER_THRESHOLD = 0.75;

export type SafetySeverity = "notice" | "alert";

export function safetyProbabilitySeverity(probability: number): SafetySeverity | null {
  if (probability >= SAFETY_ALERT_THRESHOLD) return "alert";
  if (probability >= SAFETY_NOTICE_THRESHOLD) return "notice";
  return null;
}

export function safetyCategoryPresentation(category: SafetyCategory): CategoryPresentation {
  return CATEGORY_PRESENTATION[category];
}

/** Highest visible content-safety score. Canonical order resolves ties. */
export function leadingSafetyScore(scores: SafetyScores): SafetyScore | null {
  let leading: SafetyScore | null = null;
  for (const score of orderedSafetyScores(scores)) {
    if (!isSafetyCategory(score.category) || score.probability < SAFETY_NOTICE_THRESHOLD) continue;
    if (!leading || score.probability > leading.probability) leading = score;
  }
  return leading;
}

/** Nothing is shown for a retracted message or without a visible safety score. */
export function visibleSafetyScores(
  message: Pick<TimelineMessage, "safetyScores" | "isRetracted">,
): SafetyScores | null {
  const scores = message.safetyScores;
  if (!scores || scores.scores.length === 0 || message.isRetracted) return null;
  return leadingSafetyScore(scores) ? scores : null;
}

/** A question signal is shown independently from the content-safety marker. */
export function visibleQuestionScore(
  message: Pick<TimelineMessage, "safetyScores" | "isRetracted">,
): SafetyScore | null {
  if (!message.safetyScores || message.isRetracted) return null;
  return message.safetyScores.scores.find(
    (score) => score.category === "is_question" && score.probability >= QUESTION_MARKER_THRESHOLD,
  ) ?? null;
}

/** Scores in canonical category order, independent of wire order. */
export function orderedSafetyScores(scores: SafetyScores): SafetyScore[] {
  return [...scores.scores].sort(
    (a, b) => SAFETY_CATEGORIES.indexOf(a.category) - SAFETY_CATEGORIES.indexOf(b.category),
  );
}

/** The breakdown includes safety notices and questions at their own threshold. */
export function notableSafetyScores(scores: SafetyScores): SafetyScore[] {
  return orderedSafetyScores(scores).filter((score) =>
    score.category === "is_question"
      ? score.probability >= QUESTION_MARKER_THRESHOLD
      : safetyProbabilitySeverity(score.probability) !== null,
  );
}

export function formatSafetyProbability(probability: number): string {
  const percent = Math.round(probability * 100);
  if (percent === 0 && probability > 0) return "<1%";
  return `${percent}%`;
}

export function safetyProbabilityWidth(probability: number): string {
  return `${Math.min(100, Math.max(0, probability * 100))}%`;
}
