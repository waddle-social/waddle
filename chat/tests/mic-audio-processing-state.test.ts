import { afterEach, describe, expect, test } from "bun:test";
import {
  $micAudioProcessing,
  resetMicAudioProcessing,
  setMicAudioProcessing,
} from "../src/lib/calls/mic-audio-processing-state";

afterEach(() => {
  $micAudioProcessing.set({ kind: "no-mic" });
});

describe("$micAudioProcessing store", () => {
  test("starts at no-mic before any call", () => {
    expect($micAudioProcessing.get()).toEqual({ kind: "no-mic" });
  });

  test("setMicAudioProcessing mirrors the engine's verified state", () => {
    setMicAudioProcessing({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "off",
      autoGainControl: "unknown",
    });
    expect($micAudioProcessing.get()).toEqual({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "off",
      autoGainControl: "unknown",
    });
  });

  test("resetMicAudioProcessing returns to no-mic on call end", () => {
    setMicAudioProcessing({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "on",
      autoGainControl: "on",
    });
    resetMicAudioProcessing();
    expect($micAudioProcessing.get()).toEqual({ kind: "no-mic" });
  });

  test("resetting an already-no-mic store is a no-op (no redundant emit)", () => {
    let notifications = 0;
    const unsubscribe = $micAudioProcessing.listen(() => {
      notifications += 1;
    });
    resetMicAudioProcessing();
    unsubscribe();
    expect(notifications).toBe(0);
  });

  test("setting an unchanged value does not notify (the publish double-emit)", () => {
    setMicAudioProcessing({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "on",
      autoGainControl: "on",
    });
    let notifications = 0;
    const unsubscribe = $micAudioProcessing.listen(() => {
      notifications += 1;
    });
    // Same value again — e.g. LocalTrackPublished then ActiveDeviceChanged.
    setMicAudioProcessing({
      kind: "active",
      noiseSuppression: "on",
      echoCancellation: "on",
      autoGainControl: "on",
    });
    unsubscribe();
    expect(notifications).toBe(0);
  });
});
