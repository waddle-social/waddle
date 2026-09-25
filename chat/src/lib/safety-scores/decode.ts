import type { WasmSafetyScoresFastening } from "@/lib/xmpp/wasm-types";
import { SAFETY_CATEGORIES, type SafetyCategory, type SafetyScore, type SafetyScoresFastening } from "./types";

// Decodes the wasm bridge's safety-scores fastening into the typed model.
// The Rust parser already validated the wire; this pass narrows the
// category token to `SafetyCategory` and re-checks the bounds so a
// bridge/parser drift can never render a nonsense score.

function asSafetyCategory(token: string): SafetyCategory | null {
  return (SAFETY_CATEGORIES as readonly string[]).includes(token) ? token as SafetyCategory : null;
}

function isProbability(value: number): boolean {
  return Number.isFinite(value) && value >= 0 && value <= 1;
}

function decodeScore(score: { category: string; probability: number; taxonomy_version: string }): SafetyScore | null {
  const category = asSafetyCategory(score.category);
  if (!category || !isProbability(score.probability) || !score.taxonomy_version) return null;
  return { category, probability: score.probability, taxonomyVersion: score.taxonomy_version };
}

export function safetyScoresFasteningFromWasm(fastening: WasmSafetyScoresFastening): SafetyScoresFastening | null {
  const targetId = fastening.target_id;
  if (!targetId) return null;
  if (fastening.update.kind === "clear") return { targetId, kind: "clear" };
  const { model_version: modelVersion, scores } = fastening.update;
  if (!modelVersion) return null;
  const decoded = scores.flatMap((score) => {
    const typed = decodeScore(score);
    return typed ? [typed] : [];
  });
  const unique = decoded.filter(
    (score, index) => decoded.findIndex((other) => other.category === score.category) === index,
  );
  return { targetId, kind: "replace", scores: { modelVersion, scores: unique } };
}
