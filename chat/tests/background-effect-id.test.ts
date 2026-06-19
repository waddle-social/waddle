import { describe, expect, test } from "bun:test";
import {
  BACKGROUND_OFF,
  backgroundEffectKey,
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

describe("backgroundEffectKey", () => {
  test("two custom uploads with different refs have distinct keys", () => {
    // This is what makes re-uploading a *different* image not a no-op: the
    // per-upload ref makes the new effect a distinct identity, so the reconciler
    // switches to it instead of treating it as already-applied.
    const a = backgroundEffectKey({ kind: "image", image: { source: "custom", ref: "u-1" } });
    const b = backgroundEffectKey({ kind: "image", image: { source: "custom", ref: "u-2" } });

    expect(a).not.toBe(b);
  });

  test("catalog images key on their id, blur on its own constant", () => {
    expect(backgroundEffectKey({ kind: "blur" })).toBe("blur");
    expect(
      backgroundEffectKey({ kind: "image", image: { source: "catalog", id: "office" } }),
    ).not.toBe(backgroundEffectKey({ kind: "image", image: { source: "catalog", id: "mountain" } }));
  });
});
