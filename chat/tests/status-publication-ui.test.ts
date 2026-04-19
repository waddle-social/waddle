import { describe, expect, test } from "bun:test";
import {
  buildActivityPublication,
  buildMoodPublication,
  buildTunePublication,
  formatPepKeyword,
  normalizeActivitySpecific,
} from "../src/lib/status-publication-ui";

describe("status publication UI helpers", () => {
  test("formatPepKeyword turns snake_case keys into labels", () => {
    expect(formatPepKeyword("in_awe")).toBe("In Awe");
    expect(formatPepKeyword("doing_chores")).toBe("Doing Chores");
  });

  test("buildMoodPublication requires a mood kind and trims text", () => {
    expect(buildMoodPublication({ kind: "", text: " calm " }).publication).toBeNull();
    expect(buildMoodPublication({ kind: "calm", text: " settled " }).publication).toEqual({
      kind: "calm",
      text: "settled",
    });
  });

  test("normalizeActivitySpecific tidies freeform input into snake_case", () => {
    expect(normalizeActivitySpecific(" Deep Focus! ")).toBe("deep_focus");
    expect(normalizeActivitySpecific("coffee-break")).toBe("coffee_break");
  });

  test("buildActivityPublication normalizes a specific activity and trims text", () => {
    const result = buildActivityPublication({
      general: "working",
      specific: "Deep Focus",
      text: " heads down ",
    });

    expect(result.errors).toEqual({});
    expect(result.publication).toEqual({
      general: "working",
      specific: "deep_focus",
      text: "heads down",
    });
  });

  test("buildActivityPublication rejects an empty specific slug when freeform input cannot normalize", () => {
    const result = buildActivityPublication({
      general: "working",
      specific: "!!!",
      text: "",
    });

    expect(result.publication).toBeNull();
    expect(result.errors.specific).toContain("letters or numbers");
  });

  test("buildTunePublication requires at least one detail", () => {
    const result = buildTunePublication({
      artist: "",
      title: "",
      source: "",
      length: "",
      rating: "",
      track: "",
      uri: "",
    });

    expect(result.publication).toBeNull();
    expect(result.errors.form).toContain("Add at least one tune detail");
  });

  test("buildTunePublication parses numeric fields and accepts custom URIs", () => {
    const result = buildTunePublication({
      artist: "The Beatles",
      title: "Come Together",
      source: "Abbey Road",
      length: "259",
      rating: "9",
      track: "1",
      uri: "spotify:track:2EqlS6tkEnglzr7tkKAAYD",
    });

    expect(result.errors).toEqual({});
    expect(result.publication).toEqual({
      artist: "The Beatles",
      title: "Come Together",
      source: "Abbey Road",
      length: 259,
      rating: 9,
      track: "1",
      uri: "spotify:track:2EqlS6tkEnglzr7tkKAAYD",
    });
  });

  test("buildTunePublication rejects invalid rating and URI values", () => {
    const result = buildTunePublication({
      artist: "",
      title: "Song",
      source: "",
      length: "",
      rating: "11",
      track: "",
      uri: "not a uri",
    });

    expect(result.publication).toBeNull();
    expect(result.errors.rating).toContain("between 1 and 10");
    expect(result.errors.uri).toContain("full URL or URI");
  });
});
