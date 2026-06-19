import { describe, expect, test } from "bun:test";
import {
  $devicePrefs,
  audioProcessingConstraints,
  defaultAudioProcessingPrefs,
  isSpeakerOutputSelectionSupported,
  normalizeAudioProcessingPrefs,
  parseDevicePrefsStorage,
  serializeDevicePrefsStorage,
  setAiNoiseModel,
} from "../src/lib/calls/device-prefs";

describe("call speaker output support", () => {
  test("is disabled during SSR or when media elements cannot switch sinks", () => {
    expect(isSpeakerOutputSelectionSupported({})).toBe(false);
    expect(
      isSpeakerOutputSelectionSupported({
        document: { createElement: () => ({}) },
        AudioContext: class AudioContextWithSink {
          setSinkId(): Promise<void> {
            return Promise.resolve();
          }
        },
      }),
    ).toBe(false);
  });

  test("requires Web Audio sink routing when speaker selection is shown", () => {
    const documentWithMediaSink = {
      createElement(tag: string) {
        if (tag !== "audio") throw new Error(`unexpected element: ${tag}`);
        return { setSinkId: async () => undefined };
      },
    };

    expect(
      isSpeakerOutputSelectionSupported({
        document: documentWithMediaSink,
        AudioContext: undefined,
      }),
    ).toBe(false);

    expect(
      isSpeakerOutputSelectionSupported({
        document: documentWithMediaSink,
        AudioContext: class AudioContextWithSink {
          setSinkId(): Promise<void> {
            return Promise.resolve();
          }
        },
      }),
    ).toBe(true);
  });
});

describe("call audio processing preferences", () => {
  test("default to requesting browser audio processing", () => {
    expect(defaultAudioProcessingPrefs()).toEqual({
      noiseSuppression: true,
      echoCancellation: true,
      autoGainControl: true,
    });
    expect(audioProcessingConstraints(defaultAudioProcessingPrefs())).toEqual({
      noiseSuppression: true,
      echoCancellation: true,
      autoGainControl: true,
    });
  });

  test("fall back to defaults for malformed stored values", () => {
    expect(normalizeAudioProcessingPrefs(null)).toEqual(defaultAudioProcessingPrefs());
    expect(normalizeAudioProcessingPrefs({ noiseSuppression: false })).toEqual(
      defaultAudioProcessingPrefs(),
    );
    expect(
      normalizeAudioProcessingPrefs({
        noiseSuppression: false,
        echoCancellation: false,
        autoGainControl: false,
      }),
    ).toEqual({
      noiseSuppression: false,
      echoCancellation: false,
      autoGainControl: false,
    });
  });

  test("round-trips audio processing through the persisted device prefs shape", () => {
    const stored = serializeDevicePrefsStorage({
      mic: "headset-mic",
      cam: "desk-cam",
      speaker: "usb-speaker",
      audioProcessing: {
        noiseSuppression: false,
        echoCancellation: false,
        autoGainControl: false,
      },
      aiNoiseModel: null,
      virtualBackground: { kind: "off" },
    });

    expect(parseDevicePrefsStorage(stored)).toEqual({
      mic: "headset-mic",
      cam: "desk-cam",
      speaker: "usb-speaker",
      audioProcessing: {
        noiseSuppression: false,
        echoCancellation: false,
        autoGainControl: false,
      },
      aiNoiseModel: null,
      virtualBackground: { kind: "off" },
    });
  });

  test("legacy stored device prefs default audio processing on", () => {
    expect(
      parseDevicePrefsStorage(
        JSON.stringify({
          mic: "headset-mic",
          cam: "desk-cam",
          speaker: "usb-speaker",
        }),
      ),
    ).toEqual({
      mic: "headset-mic",
      cam: "desk-cam",
      speaker: "usb-speaker",
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      virtualBackground: { kind: "off" },
    });
  });
});

describe("ai noise model preference", () => {
  test("defaults to null (off) when absent from stored prefs", () => {
    expect(
      parseDevicePrefsStorage(JSON.stringify({ mic: null, cam: null, speaker: null })).aiNoiseModel,
    ).toBeNull();
  });

  test("round-trips a selected model through the persisted shape", () => {
    const stored = serializeDevicePrefsStorage({
      mic: null,
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: "rnnoise",
      virtualBackground: { kind: "off" },
    });
    expect(parseDevicePrefsStorage(stored).aiNoiseModel).toBe("rnnoise");
  });

  test("normalizes an unknown stored model id to null (off)", () => {
    expect(
      parseDevicePrefsStorage(
        JSON.stringify({ mic: null, cam: null, speaker: null, aiNoiseModel: "bogus" }),
      ).aiNoiseModel,
    ).toBeNull();
  });

  test("normalizes a deferred model with no backend (deepfilternet) to null", () => {
    // Otherwise the engine would attempt to attach it every call and emit a
    // perpetual attach-failure notice. A user can't select it via the UI; it
    // could only reach prefs via stale/hand-edited storage.
    expect(
      parseDevicePrefsStorage(
        JSON.stringify({ mic: null, cam: null, speaker: null, aiNoiseModel: "deepfilternet" }),
      ).aiNoiseModel,
    ).toBeNull();
  });

  test("setAiNoiseModel updates the device-prefs atom", () => {
    setAiNoiseModel("dtln");
    expect($devicePrefs.get().aiNoiseModel).toBe("dtln");
    setAiNoiseModel(null);
    expect($devicePrefs.get().aiNoiseModel).toBeNull();
  });
});

describe("virtual background preference", () => {
  test("defaults to off when absent from stored prefs", () => {
    expect(
      parseDevicePrefsStorage(JSON.stringify({ mic: null, cam: null, speaker: null }))
        .virtualBackground,
    ).toEqual({ kind: "off" });
  });

  test("does not restore arbitrary or durable image URLs from storage", () => {
    expect(
      parseDevicePrefsStorage(
        JSON.stringify({
          virtualBackground: { kind: "image", imageUrl: "https://attacker.test/bg.png" },
        }),
      ).virtualBackground,
    ).toEqual({ kind: "off" });
  });

  test("serializes selected replacement images as off to avoid durable image copies", () => {
    const stored = serializeDevicePrefsStorage({
      mic: null,
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      virtualBackground: { kind: "image", imageUrl: "data:image/png;base64,ZmFrZQ==" },
    });

    expect(JSON.parse(stored).virtualBackground).toEqual({ kind: "off" });
  });
});
