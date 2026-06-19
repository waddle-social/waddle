import { describe, expect, test } from "bun:test";
import { sameCameraBackground } from "../src/lib/calls/background-effect/camera-background";

describe("sameCameraBackground", () => {
  test("two no-camera states are equal", () => {
    expect(sameCameraBackground({ kind: "no-camera" }, { kind: "no-camera" })).toBe(true);
  });

  test("no-camera differs from any active state", () => {
    expect(
      sameCameraBackground({ kind: "no-camera" }, { kind: "active", effect: { kind: "off" } }),
    ).toBe(false);
  });

  test("active states are equal iff their effects match (incl. image ref)", () => {
    const blur = { kind: "active", effect: { kind: "blur" } } as const;
    expect(sameCameraBackground(blur, { kind: "active", effect: { kind: "blur" } })).toBe(true);
    expect(
      sameCameraBackground(blur, { kind: "active", effect: { kind: "off" } }),
    ).toBe(false);

    const custom1 = {
      kind: "active",
      effect: { kind: "image", image: { source: "custom", ref: "u-1" } },
    } as const;
    const custom2 = {
      kind: "active",
      effect: { kind: "image", image: { source: "custom", ref: "u-2" } },
    } as const;
    expect(sameCameraBackground(custom1, custom2)).toBe(false);
  });
});
