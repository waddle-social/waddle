// UI for the XEP-0422 `urn:waddle:safety-scores:1` fastening: a chip on a
// scored message (visible to everyone) that stays hidden until a
// content-safety category reaches the notice threshold, turns amber there
// and red at the alert threshold, and expands inline into a breakdown of
// only the categories that crossed the notice threshold.

import { describe, expect, test } from "bun:test";
import {
  formatSafetyProbability,
  notableSafetyScores,
  orderedSafetyScores,
  safetyCategoryLabel,
  safetyProbabilityBarClass,
  safetyProbabilitySeverity,
  safetyProbabilityWidth,
  safetyScoresSeverity,
  safetyScoresToggleLabel,
  safetySeverityTextClass,
  visibleSafetyScores,
} from "../src/components/chat/message-safety-scores";
import type { SafetyScores } from "../src/lib/safety-scores/types";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { renderVueComponent, renderVueComponentSource } from "./helpers/render-vue-sfc";

/** A benign message: high question signal, every safety category low. */
const QUIET: SafetyScores = {
  modelVersion: "typesafe/jev-1.13-20260917",
  scores: [
    { category: "safety:self_harm", probability: 0, taxonomyVersion: "safety-self-harm-v1" },
    { category: "safety:hate_speech", probability: 0.003, taxonomyVersion: "safety-hate-speech-v1" },
    { category: "safety:scam", probability: 0.49, taxonomyVersion: "safety-scam-v1" },
    { category: "is_question", probability: 0.92, taxonomyVersion: "is-question-v1" },
  ],
};

/** One category at the notice threshold, another just under it. */
const NOTICE: SafetyScores = {
  modelVersion: "typesafe/jev-1.13-20260917",
  scores: [
    { category: "safety:self_harm", probability: 0, taxonomyVersion: "safety-self-harm-v1" },
    { category: "safety:harassment", probability: 0.5, taxonomyVersion: "safety-harassment-v1" },
    { category: "safety:hate_speech", probability: 0.003, taxonomyVersion: "safety-hate-speech-v1" },
    { category: "safety:scam", probability: 0.49, taxonomyVersion: "safety-scam-v1" },
    { category: "is_question", probability: 0.92, taxonomyVersion: "is-question-v1" },
  ],
};

/** One category at the alert threshold alongside a notice-level one. */
const ALERT: SafetyScores = {
  modelVersion: "typesafe/jev-1.13-20260917",
  scores: [
    { category: "safety:scam", probability: 0.8, taxonomyVersion: "safety-scam-v1" },
    { category: "safety:harassment", probability: 0.61, taxonomyVersion: "safety-harassment-v1" },
    { category: "safety:violence", probability: 0.12, taxonomyVersion: "safety-violence-v1" },
    { category: "is_question", probability: 0.1, taxonomyVersion: "is-question-v1" },
  ],
};

describe("safety-scores severity", () => {
  test("a probability is a notice from 0.5 and an alert from 0.8", () => {
    expect(safetyProbabilitySeverity(0.49)).toBeNull();
    expect(safetyProbabilitySeverity(0.5)).toBe("notice");
    expect(safetyProbabilitySeverity(0.79)).toBe("notice");
    expect(safetyProbabilitySeverity(0.8)).toBe("alert");
    expect(safetyProbabilitySeverity(1)).toBe("alert");
  });

  test("the batch severity follows the highest content-safety score only", () => {
    expect(safetyScoresSeverity(QUIET)).toBeNull();
    expect(safetyScoresSeverity(NOTICE)).toBe("notice");
    expect(safetyScoresSeverity(ALERT)).toBe("alert");
    // A near-certain question is a community signal, not a warning.
    expect(
      safetyScoresSeverity({
        modelVersion: "m",
        scores: [{ category: "is_question", probability: 1, taxonomyVersion: "v" }],
      }),
    ).toBeNull();
  });

  test("severity maps to amber or red", () => {
    expect(safetySeverityTextClass("notice")).toContain("amber");
    expect(safetySeverityTextClass("alert")).toContain("red");
    expect(safetyProbabilityBarClass(0.2)).toContain("muted");
    expect(safetyProbabilityBarClass(0.5)).toContain("amber");
    expect(safetyProbabilityBarClass(0.8)).toContain("red");
    expect(safetyScoresToggleLabel(false, "notice")).toBe("Show content notices");
    expect(safetyScoresToggleLabel(true, "alert")).toBe("Hide content alerts");
  });
});

describe("safety-scores presentation helpers", () => {
  test("only messages with a notice-level safety score show the affordance", () => {
    expect(visibleSafetyScores({ safetyScores: NOTICE })).toBe(NOTICE);
    expect(visibleSafetyScores({ safetyScores: ALERT })).toBe(ALERT);
    expect(visibleSafetyScores({ safetyScores: QUIET })).toBeNull();
    expect(visibleSafetyScores({})).toBeNull();
    expect(visibleSafetyScores({ safetyScores: { ...NOTICE, scores: [] } })).toBeNull();
    expect(visibleSafetyScores({ safetyScores: NOTICE, isRetracted: true })).toBeNull();
  });

  test("orders categories canonically regardless of wire order", () => {
    expect(orderedSafetyScores(QUIET).map((score) => score.category)).toEqual([
      "is_question",
      "safety:hate_speech",
      "safety:self_harm",
      "safety:scam",
    ]);
  });

  test("the breakdown lists only categories at or above the notice threshold", () => {
    expect(notableSafetyScores(NOTICE).map((score) => score.category)).toEqual([
      "is_question",
      "safety:harassment",
    ]);
    expect(notableSafetyScores(ALERT).map((score) => score.category)).toEqual([
      "safety:harassment",
      "safety:scam",
    ]);
    expect(notableSafetyScores(QUIET)).toEqual([
      { category: "is_question", probability: 0.92, taxonomyVersion: "is-question-v1" },
    ]);
  });

  test("formats probabilities as whole percents without faking zeros", () => {
    expect(formatSafetyProbability(0.92)).toBe("92%");
    expect(formatSafetyProbability(0)).toBe("0%");
    expect(formatSafetyProbability(0.003)).toBe("<1%");
    expect(formatSafetyProbability(1)).toBe("100%");
    expect(safetyProbabilityWidth(0.92)).toBe("92%");
    expect(safetyProbabilityWidth(1.5)).toBe("100%");
  });

  test("labels every known category in plain language", () => {
    expect(safetyCategoryLabel("is_question")).toBe("Question");
    expect(safetyCategoryLabel("safety:self_harm")).toBe("Self-harm");
    expect(safetyCategoryLabel("safety:spam")).toBe("Spam");
    expect(safetyCategoryLabel("safety:scam")).toBe("Scam");
  });
});

describe("MessageSafetyScores", () => {
  const componentUrl = new URL("../src/components/chat/MessageSafetyScores.vue", import.meta.url);

  test("renders collapsed: an amber toggle for a notice and no breakdown", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageSafetyScores.vue",
      { scores: NOTICE, messageId: "m-1" },
      import.meta.url,
    );
    expect(html).toContain("Scores");
    expect(html).toContain('data-severity="notice"');
    expect(html).toContain("text-amber-600");
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('aria-controls="safety-scores-m-1"');
    expect(html).toContain('aria-label="Show content notices"');
    expect(html).not.toContain("Harassment");
  });

  test("renders a red toggle for an alert", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageSafetyScores.vue",
      { scores: ALERT, messageId: "m-2" },
      import.meta.url,
    );
    expect(html).toContain('data-severity="alert"');
    expect(html).toContain("text-red-600");
    expect(html).toContain('aria-label="Show content alerts"');
  });

  test("the expanded panel lists only notice-level categories, their percent, and the model", async () => {
    // Render the same SFC with its disclosure state flipped open; SSR has
    // no click, and the component deliberately exposes no prop for it.
    const source = (await Bun.file(componentUrl).text()).replace(
      "const expanded = ref(false);",
      "const expanded = ref(true);",
    );
    expect(source).toContain("const expanded = ref(true);");
    const html = await renderVueComponentSource(source, { scores: ALERT, messageId: "m-1" });

    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain('id="safety-scores-m-1"');
    const harassment = html.indexOf("Harassment");
    const scam = html.indexOf("Scam");
    expect(harassment).toBeGreaterThan(-1);
    expect(scam).toBeGreaterThan(harassment);
    expect(html).not.toContain("Spam");
    expect(html).not.toContain("Violence");
    expect(html).not.toContain("Question");
    expect(html).toContain("61%");
    expect(html).toContain("80%");
    expect(html).not.toContain("12%");
    expect(html).toContain("bg-amber-500/80");
    expect(html).toContain("bg-red-500/80");
    expect(html).toContain('title="safety-scam-v1"');
    expect(html).toContain("Model typesafe/jev-1.13-20260917");
  });
});

describe("MessageCard safety-scores affordance", () => {
  const base: TimelineMessage = {
    id: "m-1",
    author: "bob",
    body: "is anyone around?",
    createdAt: "2026-09-25T10:00:00Z",
    createdAtSource: "archive",
    isSelf: false,
  };

  // One wrapper renders all four cards so MessageCard compiles once.
  const wrapper = `<script setup lang="ts">
import MessageCard from "@/components/chat/MessageCard.vue";
import type { TimelineMessage } from "@/lib/chat-ui";
defineProps<{ scored: TimelineMessage; grouped: TimelineMessage; quiet: TimelineMessage; plain: TimelineMessage }>();
</script>
<template>
  <div>
    <MessageCard :message="scored" :hats="[]" />
    <MessageCard :message="grouped" :hats="[]" :grouped="true" />
    <MessageCard :message="quiet" :hats="[]" />
    <MessageCard :message="plain" :hats="[]" />
  </div>
</template>
`;

  test("notice-level messages show the chip, including grouped rows; quiet and unscored rows show none", async () => {
    const html = await renderVueComponentSource(wrapper, {
      scored: { ...base, id: "scored", safetyScores: NOTICE },
      grouped: { ...base, id: "grouped", safetyScores: ALERT },
      quiet: { ...base, id: "quiet", safetyScores: QUIET },
      plain: { ...base, id: "plain" },
    });
    expect(html).toContain('aria-controls="safety-scores-scored"');
    expect(html).toContain('aria-controls="safety-scores-grouped"');
    expect(html).not.toContain("safety-scores-quiet");
    expect(html).not.toContain("safety-scores-plain");
  }, 30_000);
});
