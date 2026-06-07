import { describe, expect, test } from "bun:test";
import {
  activeMicAudioProcessing,
  audioProcessingRows,
  constraintState,
  type MicAudioProcessing,
} from "../src/lib/calls/mic-audio-processing";

describe("constraintState — honest tri-state from getSettings", () => {
  test("true maps to on (requested AND applied)", () => {
    expect(constraintState(true)).toBe("on");
  });

  test("false maps to off (requested but the device/browser refused)", () => {
    expect(constraintState(false)).toBe("off");
  });

  test("an absent field maps to unknown, NOT off", () => {
    // The headline correctness property: a browser that omits the field
    // (Safari/Firefox often do) must read as `unknown`, never `off` —
    // otherwise the indicator cries wolf on a probably-working mic.
    expect(constraintState(undefined)).toBe("unknown");
  });

  test("an echo-cancellation mode string ('all'/'remote-only') reads as on", () => {
    // `MediaTrackSettings.echoCancellation` is `boolean | string`: some
    // browsers report the active *mode* rather than a bare boolean. A
    // present mode means echo cancellation is on.
    expect(constraintState("all")).toBe("on");
    expect(constraintState("remote-only")).toBe("on");
    expect(constraintState("")).toBe("off");
  });
});

describe("activeMicAudioProcessing — reads the whole APM trio", () => {
  test("maps each constraint independently from one settings snapshot", () => {
    const state = activeMicAudioProcessing({
      noiseSuppression: true,
      echoCancellation: false,
      // autoGainControl intentionally absent → unknown
    } as MediaTrackSettings);
    expect(state).toEqual({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "off",
      autoGainControl: "unknown",
    });
  });

  test("all-applied settings produce an all-on active state", () => {
    const state = activeMicAudioProcessing({
      noiseSuppression: true,
      echoCancellation: true,
      autoGainControl: true,
    } as MediaTrackSettings);
    expect(state.noiseSuppression).toBe("on");
    expect(state.echoCancellation).toBe("on");
    expect(state.autoGainControl).toBe("on");
  });
});

describe("audioProcessingRows — tiered presentation", () => {
  const rows = audioProcessingRows({
    kind: "active",
    noiseSuppression: "on",
    echoCancellation: "off",
    autoGainControl: "unknown",
  });

  test("renders noise cancellation first as the headline", () => {
    expect(rows.map((r) => r.key)).toEqual([
      "noiseSuppression",
      "echoCancellation",
      "autoGainControl",
    ]);
    expect(rows[0]?.label).toBe("Noise cancellation");
  });

  test("on → calm tone, 'On' label, no caption", () => {
    const ns = rows[0];
    expect(ns?.state).toBe("on");
    expect(ns?.tone).toBe("on");
    expect(ns?.stateLabel).toBe("On");
    expect(ns?.detail).toBeNull();
  });

  test("off → warn tone with an explanatory caption (a real degradation)", () => {
    const echo = rows[1];
    expect(echo?.state).toBe("off");
    expect(echo?.tone).toBe("warn");
    expect(echo?.stateLabel).toBe("Off");
    expect(echo?.detail).toBeTruthy();
  });

  test("unknown → muted tone, 'Not reported', browser-doesn't-report caption", () => {
    const agc = rows[2];
    expect(agc?.state).toBe("unknown");
    expect(agc?.tone).toBe("muted");
    expect(agc?.stateLabel).toBe("Not reported");
    expect(agc?.detail).toContain("browser");
  });
});

describe("MicAudioProcessing — discriminated union keeps no-mic distinct", () => {
  test("a no-mic value carries no constraint fields to misread", () => {
    const state: MicAudioProcessing = { kind: "no-mic" };
    // Type-level: the UI must branch on `kind` before reading a
    // constraint. Runtime guard that the union stays a 2-arm shape.
    expect(state.kind).toBe("no-mic");
    expect("noiseSuppression" in state).toBe(false);
  });
});
