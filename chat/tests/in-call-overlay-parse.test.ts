import { describe, expect, test } from "bun:test";

import { inCallFromActivity, parseActivityOverlay } from "../src/presence/in-call-activity";
import { withFakeDomParser } from "./helpers/disco-xml";

const NS = "http://jabber.org/protocol/activity";

function activityXml(general?: string, specific?: string): string {
  const inner = general
    ? `<${general}>${specific ? `<${specific}/>` : ""}</${general}>`
    : "";
  return `<activity xmlns="${NS}">${inner}</activity>`;
}

describe("parseActivityOverlay (incoming XEP-0108 → in-call)", () => {
  test("talking/on_the_phone reads as in a call", async () => {
    await withFakeDomParser(async () => {
      expect(parseActivityOverlay(activityXml("talking", "on_the_phone"))).toBe(true);
    });
  });

  test("talking/on_video_phone reads as in a call", async () => {
    await withFakeDomParser(async () => {
      expect(parseActivityOverlay(activityXml("talking", "on_video_phone"))).toBe(true);
    });
  });

  test("the empty activity retraction clears the overlay", async () => {
    await withFakeDomParser(async () => {
      expect(parseActivityOverlay(activityXml())).toBe(false);
    });
  });

  test("a non-call activity (working/coding) is not in a call", async () => {
    await withFakeDomParser(async () => {
      expect(parseActivityOverlay(activityXml("working", "coding"))).toBe(false);
    });
  });

  test("talking without an on-the-phone specific is not in a call", async () => {
    await withFakeDomParser(async () => {
      expect(parseActivityOverlay(activityXml("talking"))).toBe(false);
      expect(parseActivityOverlay(activityXml("talking", "in_real_life"))).toBe(false);
    });
  });
});

describe("inCallFromActivity (pure mapper)", () => {
  test("talking + on_the_phone / on_video_phone are in a call", () => {
    expect(inCallFromActivity("talking", "on_the_phone")).toBe(true);
    expect(inCallFromActivity("talking", "on_video_phone")).toBe(true);
  });

  test("talking with another or no specific is not in a call", () => {
    expect(inCallFromActivity("talking", "in_real_life")).toBe(false);
    expect(inCallFromActivity("talking", null)).toBe(false);
  });

  test("a different general activity is never in a call", () => {
    expect(inCallFromActivity("working", "on_the_phone")).toBe(false);
    expect(inCallFromActivity("relaxing", null)).toBe(false);
  });
});
