import { afterEach, describe, expect, test } from "bun:test";
import {
  $callConnectionPhase,
  $callConnectionQuality,
  qualityToChip,
  resetCallConnectionQuality,
  setCallConnectionPhase,
  setCallConnectionQuality,
} from "../src/lib/calls/connection-quality";

afterEach(() => {
  resetCallConnectionQuality();
});

describe("qualityToChip — ambient self-quality rendering model", () => {
  test("excellent and good show full/strong bars with no nagging label", () => {
    expect(qualityToChip("excellent", "connected")).toEqual({
      bars: 3,
      tone: "neutral",
      label: null,
    });
    expect(qualityToChip("good", "connected")).toEqual({
      bars: 2,
      tone: "neutral",
      label: null,
    });
  });

  test("poor escalates to a single amber bar with a label", () => {
    expect(qualityToChip("poor", "connected")).toEqual({
      bars: 1,
      tone: "warn",
      label: "Poor connection",
    });
  });

  test("lost shows empty red bars labelled Reconnecting…", () => {
    expect(qualityToChip("lost", "connected")).toEqual({
      bars: 0,
      tone: "danger",
      label: "Reconnecting…",
    });
  });

  test("unknown hides the indicator entirely (null), so a fresh call never flashes empty bars", () => {
    expect(qualityToChip("unknown", "connected")).toBeNull();
    expect(qualityToChip("unknown", "disconnected")).toBeNull();
  });

  test("a reconnecting transport overrides the last quality sample", () => {
    // Even if the last score was excellent, a re-establishing path must
    // read as Reconnecting… — the bars are meaningless while it's down.
    expect(qualityToChip("excellent", "reconnecting")).toEqual({
      bars: 0,
      tone: "danger",
      label: "Reconnecting…",
    });
    expect(qualityToChip("poor", "reconnecting")).toEqual({
      bars: 0,
      tone: "danger",
      label: "Reconnecting…",
    });
  });
});

describe("connection-quality atoms", () => {
  test("setters update the atoms and reset returns to the no-call baseline", () => {
    setCallConnectionQuality("poor");
    setCallConnectionPhase("reconnecting");
    expect($callConnectionQuality.get()).toBe("poor");
    expect($callConnectionPhase.get()).toBe("reconnecting");

    resetCallConnectionQuality();
    expect($callConnectionQuality.get()).toBe("unknown");
    expect($callConnectionPhase.get()).toBe("disconnected");
    // …and the chip is hidden again, so nothing bleeds into the next call.
    expect(qualityToChip($callConnectionQuality.get(), $callConnectionPhase.get())).toBeNull();
  });
});
