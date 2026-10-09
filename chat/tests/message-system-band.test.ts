import { renderVueComponent } from "./helpers/render-vue-sfc";
import { describe, expect, test } from "bun:test";
import {
  eventBandsFor,
  rendersAsSystemBand,
  systemBandKindClass,
  systemBandMetaValueClass,
  systemBandToneClass,
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

describe("system band class helpers", () => {
  test("tone classes map success/danger/warning and default to none", () => {
    expect(systemBandToneClass("success")).toBe("chat-system-band--tone-success");
    expect(systemBandToneClass("danger")).toBe("chat-system-band--tone-danger");
    expect(systemBandToneClass("warning")).toBe("chat-system-band--tone-warning");
    expect(systemBandToneClass("neutral")).toBe("");
  });

  test("kind class is per payload shape", () => {
    expect(systemBandKindClass("github-event")).toBe("chat-system-band--kind-github-event");
    expect(systemBandKindClass("")).toBe("");
  });

  test("commit and branch values get the tabular-mono treatment", () => {
    expect(systemBandMetaValueClass("Commit")).toBe("chat-system-band__meta-value--mono");
    expect(systemBandMetaValueClass("Branch")).toBe("chat-system-band__meta-value--mono");
    expect(systemBandMetaValueClass("Event")).toBe("");
  });
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
