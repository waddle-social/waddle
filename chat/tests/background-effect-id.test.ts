import { describe, expect, test } from "bun:test";
import {
  BACKGROUND_OFF,
  normalizeBackgroundEffect,
} from "../src/lib/calls/background-effect/effect-id";

describe("normalizeBackgroundEffect", () => {
  test("keeps a valid off / blur effect", () => {
    expect(normalizeBackgroundEffect({ kind: "off" })).toEqual(BACKGROUND_OFF);
    expect(normalizeBackgroundEffect({ kind: "blur" })).toEqual({ kind: "blur" });
  });

  test("keeps a catalog image with a known id", () => {
    const effect = { kind: "image", image: { source: "catalog", id: "office" } };

    expect(normalizeBackgroundEffect(effect)).toEqual(effect);
  });

  test("keeps a custom image with a non-empty ref", () => {
    const effect = { kind: "image", image: { source: "custom", ref: "u-7" } };

    expect(normalizeBackgroundEffect(effect)).toEqual(effect);
  });

  test("falls back to off for a catalog image with an unknown id", () => {
    const effect = { kind: "image", image: { source: "catalog", id: "lava-lamp" } };

    expect(normalizeBackgroundEffect(effect)).toEqual(BACKGROUND_OFF);
  });

  test("falls back to off for a custom image missing its ref", () => {
    const effect = { kind: "image", image: { source: "custom" } };

    expect(normalizeBackgroundEffect(effect)).toEqual(BACKGROUND_OFF);
  });

  test("falls back to off for junk", () => {
    expect(normalizeBackgroundEffect(null)).toEqual(BACKGROUND_OFF);
    expect(normalizeBackgroundEffect({ kind: "wat" })).toEqual(BACKGROUND_OFF);
    expect(normalizeBackgroundEffect("blur")).toEqual(BACKGROUND_OFF);
  });
});
