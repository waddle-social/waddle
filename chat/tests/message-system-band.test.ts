import { renderVueComponent } from "./helpers/render-vue-sfc";
import { describe, expect, test } from "bun:test";
import {
  eventBandsFor,
  rendersAsSystemBand,
  isMonoBandMeta,
} from "../src/components/chat/message-system-band";
import type { ExtensionAnnotation } from "../src/lib/chat-ui";

function annotation(overrides: Partial<ExtensionAnnotation> = {}): ExtensionAnnotation {
  return {
    extensionId: "ext-1",
    annotationId: "ann-1",
    surfaceKind: "chat-bot",
    title: "Deploy finished",
    fields: {},
    actions: [],
    ...overrides,
  };
}

describe("eventBandsFor", () => {
  test("keeps event-intent annotations and drops tool-intent ones", () => {
    const event = annotation();
    const tool = annotation({
      annotationId: "ann-2",
      actions: [{ label: "Save", route: "save" }],
    });
    const bands = eventBandsFor([event, tool]);
    expect(bands.map((band) => band.annotation.annotationId)).toEqual(["ann-1"]);
    expect(bands[0]?.presentation.intent).toBe("event");
  });

  test("returns empty for missing annotations", () => {
    expect(eventBandsFor(undefined)).toEqual([]);
  });
});

describe("rendersAsSystemBand", () => {
  test("true only when an event band declares the chat-bot surface", () => {
    expect(rendersAsSystemBand(eventBandsFor([annotation()]))).toBe(true);
    expect(rendersAsSystemBand(eventBandsFor([annotation({ surfaceKind: "message-card" })]))).toBe(false);
    expect(rendersAsSystemBand([])).toBe(false);
  });
});

test("commit and branch values read as code", () => {
  expect(isMonoBandMeta("Commit")).toBe(true);
  expect(isMonoBandMeta("Branch")).toBe(true);
  expect(isMonoBandMeta("Event")).toBe(false);
});

test("system-band message cards expose the same local presentation anchor as ordinary cards", async () => {
  const html = await renderVueComponent("../src/components/chat/MessageCard.vue", {
    message: {
      id: "wire-id", rowKey: "stable-system-row", author: "bot", body: "event",
      createdAt: "2026-07-01T10:00:00Z", isSelf: false,
      extensionAnnotations: [annotation()],
    },
    hats: [],
  }, import.meta.url);
  expect(html).toContain('data-message-row-key="stable-system-row"');
  expect(html).toContain('data-message-id="wire-id"');
}, 30_000);
