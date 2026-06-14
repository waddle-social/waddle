import { describe, expect, test } from "bun:test";
import type { AiNoiseFilterState } from "../src/lib/calls/ai-noise-filter/mic-ai-noise-filter";
import type { MicAudioProcessing } from "../src/lib/calls/mic-audio-processing";
import {
  callAudioProcessingEventAttributes,
  createCallAudioProcessingBeacon,
  type VerifiedCallAudioProcessing,
} from "../src/lib/calls/call-audio-processing-telemetry";

const ACTIVE_ON: MicAudioProcessing = {
  kind: "active",
  noiseSuppression: "on",
  echoCancellation: "on",
  autoGainControl: "on",
};

const FILTER_OFF: AiNoiseFilterState = { kind: "active", model: null };
const FILTER_RNNOISE: AiNoiseFilterState = { kind: "active", model: "rnnoise" };

const snap = (
  processing: MicAudioProcessing,
  aiNoiseFilter: AiNoiseFilterState,
): VerifiedCallAudioProcessing => ({ processing, aiNoiseFilter });

describe("callAudioProcessingEventAttributes", () => {
  test("maps an active mic with no AI filter to tri-state + ai_noise_filter=off", () => {
    expect(
      callAudioProcessingEventAttributes(
        snap(
          { kind: "active", noiseSuppression: "on", echoCancellation: "off", autoGainControl: "unknown" },
          FILTER_OFF,
        ),
      ),
    ).toEqual({
      kind: "active",
      noise_suppression: "on",
      echo_cancellation: "off",
      auto_gain_control: "unknown",
      ai_noise_filter: "off",
    });
  });

  test("reports the active model id in ai_noise_filter", () => {
    // A model supersedes the browser NS, so noise_suppression reads off here.
    const attrs = callAudioProcessingEventAttributes(
      snap({ ...ACTIVE_ON, noiseSuppression: "off" }, FILTER_RNNOISE),
    );
    expect(attrs.ai_noise_filter).toBe("rnnoise");
    expect(attrs.noise_suppression).toBe("off");
  });

  test("maps a no-mic state to its kind with ai_noise_filter=off", () => {
    expect(callAudioProcessingEventAttributes(snap({ kind: "no-mic" }, { kind: "no-mic" }))).toEqual({
      kind: "no-mic",
      ai_noise_filter: "off",
    });
  });

  test("never emits device identifiers, labels, or JIDs", () => {
    const attrs = callAudioProcessingEventAttributes(snap(ACTIVE_ON, FILTER_RNNOISE));
    const allowed = new Set([
      "kind",
      "noise_suppression",
      "echo_cancellation",
      "auto_gain_control",
      "ai_noise_filter",
    ]);
    for (const key of Object.keys(attrs)) {
      expect(allowed.has(key)).toBe(true);
    }
    const serialized = JSON.stringify(attrs).toLowerCase();
    expect(serialized).not.toContain("device");
    expect(serialized).not.toContain("@");
    expect(serialized).not.toContain("label");
  });
});

describe("createCallAudioProcessingBeacon", () => {
  test("reports a state once, not again for an equal recompute", () => {
    const reported: VerifiedCallAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(snap(ACTIVE_ON, FILTER_OFF));
    beacon.observe(snap({ ...ACTIVE_ON }, { ...FILTER_OFF }));

    expect(reported).toHaveLength(1);
  });

  test("re-beacons when only the AI filter changes", () => {
    const reported: VerifiedCallAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(snap(ACTIVE_ON, FILTER_OFF));
    beacon.observe(snap(ACTIVE_ON, FILTER_RNNOISE));

    expect(reported).toHaveLength(2);
    expect(reported[1]?.aiNoiseFilter).toEqual(FILTER_RNNOISE);
  });

  test("re-beacons when the browser-constraint state changes", () => {
    const reported: VerifiedCallAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(snap(ACTIVE_ON, FILTER_OFF));
    beacon.observe(snap({ ...ACTIVE_ON, noiseSuppression: "off" }, FILTER_OFF));

    expect(reported).toHaveLength(2);
  });

  test("does not re-beacon a snapshot already seen earlier in the same call", () => {
    const reported: VerifiedCallAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(snap(ACTIVE_ON, FILTER_OFF));
    beacon.observe(snap({ kind: "no-mic" }, { kind: "no-mic" }));
    beacon.observe(snap({ ...ACTIVE_ON }, { ...FILTER_OFF }));

    expect(reported).toHaveLength(2);
  });

  test("reset re-arms the beacon for the next call", () => {
    const reported: VerifiedCallAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(snap(ACTIVE_ON, FILTER_OFF));
    beacon.reset();
    beacon.observe(snap({ ...ACTIVE_ON }, { ...FILTER_OFF }));

    expect(reported).toHaveLength(2);
  });
});
