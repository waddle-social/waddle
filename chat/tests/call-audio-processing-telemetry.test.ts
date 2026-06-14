import { describe, expect, test } from "bun:test";
import type { MicAudioProcessing } from "../src/lib/calls/mic-audio-processing";
import {
  callAudioProcessingEventAttributes,
  createCallAudioProcessingBeacon,
} from "../src/lib/calls/call-audio-processing-telemetry";

const ACTIVE_ON: MicAudioProcessing = {
  kind: "active",
  noiseSuppression: "on",
  echoCancellation: "on",
  autoGainControl: "on",
};

describe("callAudioProcessingEventAttributes", () => {
  test("maps an active mic to snake_case tri-state attributes", () => {
    expect(
      callAudioProcessingEventAttributes({
        kind: "active",
        noiseSuppression: "on",
        echoCancellation: "off",
        autoGainControl: "unknown",
      }),
    ).toEqual({
      kind: "active",
      noise_suppression: "on",
      echo_cancellation: "off",
      auto_gain_control: "unknown",
    });
  });

  test("maps a no-mic state to just its kind", () => {
    expect(callAudioProcessingEventAttributes({ kind: "no-mic" })).toEqual({
      kind: "no-mic",
    });
  });

  test("never emits device identifiers, labels, or JIDs", () => {
    const attrs = callAudioProcessingEventAttributes({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "on",
      autoGainControl: "on",
    });
    const allowed = new Set([
      "kind",
      "noise_suppression",
      "echo_cancellation",
      "auto_gain_control",
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
    const reported: MicAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(ACTIVE_ON);
    beacon.observe({ ...ACTIVE_ON });

    expect(reported).toEqual([ACTIVE_ON]);
  });

  test("reports again when the verified state actually changes", () => {
    const reported: MicAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));
    const nsOff: MicAudioProcessing = { ...ACTIVE_ON, noiseSuppression: "off" };

    beacon.observe(ACTIVE_ON);
    beacon.observe(nsOff);

    expect(reported).toEqual([ACTIVE_ON, nsOff]);
  });

  test("does not re-beacon a state already seen earlier in the same call", () => {
    const reported: MicAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));
    const noMic: MicAudioProcessing = { kind: "no-mic" };

    // Mute/unmute (or a device reconnect) cycles active → no-mic → active.
    // The returning `active` state is a duplicate within the call and must
    // not produce a second beacon (acceptance criterion: "Redundant/
    // duplicate states do not produce repeated beacons within a call").
    beacon.observe(ACTIVE_ON);
    beacon.observe(noMic);
    beacon.observe({ ...ACTIVE_ON });

    expect(reported).toEqual([ACTIVE_ON, noMic]);
  });

  test("reset re-arms the beacon for the next call", () => {
    const reported: MicAudioProcessing[] = [];
    const beacon = createCallAudioProcessingBeacon((state) => reported.push(state));

    beacon.observe(ACTIVE_ON);
    beacon.reset();
    beacon.observe({ ...ACTIVE_ON });

    expect(reported).toHaveLength(2);
  });
});
