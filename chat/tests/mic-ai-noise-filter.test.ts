import { describe, expect, test } from "bun:test";
import {
  activeAiNoiseFilter,
  aiNoiseFilterRow,
  sameAiNoiseFilter,
  type AiNoiseFilterState,
} from "../src/lib/calls/ai-noise-filter/mic-ai-noise-filter";
import { processorName } from "../src/lib/calls/ai-noise-filter/model-id";

describe("activeAiNoiseFilter — verified model from the live processor name", () => {
  test("reports the attached model when one of ours is running", () => {
    expect(activeAiNoiseFilter(processorName("dtln"))).toEqual({
      kind: "active",
      model: "dtln",
    });
  });

  test("reports model=null (filter off) when no processor is attached", () => {
    // A live mic with no AI processor: the indicator says "Off", honestly.
    expect(activeAiNoiseFilter(undefined)).toEqual({ kind: "active", model: null });
  });

  test("reports model=null for a foreign processor (not ours)", () => {
    expect(activeAiNoiseFilter("lk-krisp-noise-filter")).toEqual({
      kind: "active",
      model: null,
    });
  });
});

describe("sameAiNoiseFilter — dedup equal recomputes", () => {
  test("two no-mic states are equal", () => {
    expect(sameAiNoiseFilter({ kind: "no-mic" }, { kind: "no-mic" })).toBe(true);
  });

  test("active states are equal iff the model matches", () => {
    const a: AiNoiseFilterState = { kind: "active", model: "rnnoise" };
    expect(sameAiNoiseFilter(a, { kind: "active", model: "rnnoise" })).toBe(true);
    expect(sameAiNoiseFilter(a, { kind: "active", model: "dtln" })).toBe(false);
  });

  test("no-mic and active are never equal", () => {
    expect(sameAiNoiseFilter({ kind: "no-mic" }, { kind: "active", model: null })).toBe(false);
  });
});

describe("aiNoiseFilterRow — indicator row for the settings dialog", () => {
  test("an active model shows its tier·name label with a positive tone", () => {
    const row = aiNoiseFilterRow({ kind: "active", model: "deepfilternet" });
    expect(row.label).toBe("AI noise filter");
    expect(row.stateLabel).toBe("Maximum · DeepFilterNet");
    expect(row.tone).toBe("on");
  });

  test("filter off shows 'Off' with a muted tone", () => {
    const row = aiNoiseFilterRow({ kind: "active", model: null });
    expect(row.stateLabel).toBe("Off");
    expect(row.tone).toBe("muted");
  });
});
