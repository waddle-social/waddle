// UI for the XEP-0422 `urn:waddle:safety-scores:1` fastening: a quiet
// "Scores" chip on every scored message (visible to everyone) that
// expands inline into the per-category breakdown.

import { describe, expect, test } from "bun:test";
import {
  formatSafetyProbability,
  orderedSafetyScores,
  safetyCategoryLabel,
  safetyProbabilityWidth,
  visibleSafetyScores,
} from "../src/components/chat/message-safety-scores";
import type { SafetyScores } from "../src/lib/safety-scores/types";
import type { TimelineMessage } from "../src/lib/chat-ui";
import { renderVueComponent, renderVueComponentSource } from "./helpers/render-vue-sfc";

const SCORES: SafetyScores = {
  modelVersion: "typesafe/jev-1.13-20260917",
  scores: [
    { category: "safety:self_harm", probability: 0, taxonomyVersion: "safety-self-harm-v1" },
    { category: "safety:hate_speech", probability: 0.003, taxonomyVersion: "safety-hate-speech-v1" },
    { category: "is_question", probability: 0.92, taxonomyVersion: "is-question-v1" },
  ],
};

describe("safety-scores presentation helpers", () => {
  test("only scored, non-retracted messages show the affordance", () => {
    expect(visibleSafetyScores({ safetyScores: SCORES })).toBe(SCORES);
    expect(visibleSafetyScores({})).toBeNull();
    expect(visibleSafetyScores({ safetyScores: { ...SCORES, scores: [] } })).toBeNull();
    expect(visibleSafetyScores({ safetyScores: SCORES, isRetracted: true })).toBeNull();
  });

  test("orders categories canonically regardless of wire order", () => {
    expect(orderedSafetyScores(SCORES).map((score) => score.category)).toEqual([
      "is_question",
      "safety:hate_speech",
      "safety:self_harm",
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
  });
});

describe("MessageSafetyScores", () => {
  const componentUrl = new URL("../src/components/chat/MessageSafetyScores.vue", import.meta.url);

  test("renders collapsed: a labelled toggle and no breakdown", async () => {
    const html = await renderVueComponent(
      "../src/components/chat/MessageSafetyScores.vue",
      { scores: SCORES, messageId: "m-1" },
      import.meta.url,
    );
    expect(html).toContain("Scores");
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('aria-controls="safety-scores-m-1"');
    expect(html).toContain('aria-label="Show content scores"');
    expect(html).not.toContain("Hate speech");
  });

  test("the expanded panel lists each category, its percent, and the model", async () => {
    // Render the same SFC with its disclosure state flipped open; SSR has
    // no click, and the component deliberately exposes no prop for it.
    const source = (await Bun.file(componentUrl).text()).replace(
      "const expanded = ref(false);",
      "const expanded = ref(true);",
    );
    expect(source).toContain("const expanded = ref(true);");
    const html = await renderVueComponentSource(source, { scores: SCORES, messageId: "m-1" });

    expect(html).toContain('aria-expanded="true"');
    expect(html).toContain('id="safety-scores-m-1"');
    const question = html.indexOf("Question");
    const hate = html.indexOf("Hate speech");
    const selfHarm = html.indexOf("Self-harm");
    expect(question).toBeGreaterThan(-1);
    expect(hate).toBeGreaterThan(question);
    expect(selfHarm).toBeGreaterThan(hate);
    expect(html).toContain("92%");
    expect(html).toContain("&lt;1%");
    expect(html).toContain('title="is-question-v1"');
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

  // One wrapper renders all three cards so MessageCard compiles once.
  const wrapper = `<script setup lang="ts">
import MessageCard from "@/components/chat/MessageCard.vue";
import type { TimelineMessage } from "@/lib/chat-ui";
defineProps<{ scored: TimelineMessage; grouped: TimelineMessage; plain: TimelineMessage }>();
</script>
<template>
  <div>
    <MessageCard :message="scored" :hats="[]" />
    <MessageCard :message="grouped" :hats="[]" :grouped="true" />
    <MessageCard :message="plain" :hats="[]" />
  </div>
</template>
`;

  test("every scored message shows the chip, including grouped rows; unscored rows show none", async () => {
    const html = await renderVueComponentSource(wrapper, {
      scored: { ...base, id: "scored", safetyScores: SCORES },
      grouped: { ...base, id: "grouped", safetyScores: SCORES },
      plain: { ...base, id: "plain" },
    });
    expect(html).toContain('aria-controls="safety-scores-scored"');
    expect(html).toContain('aria-controls="safety-scores-grouped"');
    expect(html).not.toContain("safety-scores-plain");
  }, 30_000);
});
