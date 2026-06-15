import { describe, expect, test } from "bun:test";
import { effectiveAudioProcessing } from "../src/lib/calls/ai-noise-filter/effective-audio-processing";
import type { AudioProcessingPrefs } from "../src/lib/calls/device-prefs";

const allOn: AudioProcessingPrefs = {
  noiseSuppression: true,
  echoCancellation: true,
  autoGainControl: true,
};

describe("effectiveAudioProcessing — capture constraints actually requested", () => {
  test("with no AI model, the stored prefs pass through unchanged", () => {
    expect(effectiveAudioProcessing(allOn, null)).toEqual(allOn);
  });

  test("with an AI model active, browser noise suppression is forced off", () => {
    // Two noise suppressors in series fight each other and the browser NS
    // mangles the signal the model was trained on — so the model replaces it.
    const effective = effectiveAudioProcessing(allOn, "rnnoise");
    expect(effective.noiseSuppression).toBe(false);
  });

  test("echo cancellation and auto gain are left under the user's control", () => {
    // AEC needs the far-end reference only WebRTC has; AGC is level control.
    // The noise models do neither, so those toggles stay as stored.
    const effective = effectiveAudioProcessing(allOn, "deepfilternet");
    expect(effective.echoCancellation).toBe(true);
    expect(effective.autoGainControl).toBe(true);
  });

  test("a stored noiseSuppression=false is unaffected by activating a model", () => {
    const off: AudioProcessingPrefs = { ...allOn, noiseSuppression: false };
    expect(effectiveAudioProcessing(off, "dtln").noiseSuppression).toBe(false);
  });

  test("does not mutate the input prefs (override is pure)", () => {
    const input: AudioProcessingPrefs = { ...allOn };
    effectiveAudioProcessing(input, "rnnoise");
    expect(input.noiseSuppression).toBe(true);
  });
});
