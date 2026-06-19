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
  setBackgroundEffectPref,
} from "../src/lib/calls/device-prefs";
import { BACKGROUND_OFF } from "../src/lib/calls/background-effect/effect-id";

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
      backgroundEffect: BACKGROUND_OFF,
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
      backgroundEffect: BACKGROUND_OFF,
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
      backgroundEffect: BACKGROUND_OFF,
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
      backgroundEffect: BACKGROUND_OFF,
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

describe("background effect preference", () => {
  test("defaults to off when absent from stored prefs", () => {
    expect(
      parseDevicePrefsStorage(JSON.stringify({ mic: null, cam: null, speaker: null }))
        .backgroundEffect,
    ).toEqual(BACKGROUND_OFF);
  });

  test("round-trips a catalog image through the persisted shape", () => {
    const stored = serializeDevicePrefsStorage({
      mic: null,
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      backgroundEffect: { kind: "image", image: { source: "catalog", id: "office" } },
    });
    expect(parseDevicePrefsStorage(stored).backgroundEffect).toEqual({
      kind: "image",
      image: { source: "catalog", id: "office" },
    });
  });

  test("normalizes a malformed stored effect to off", () => {
    expect(
      parseDevicePrefsStorage(
        JSON.stringify({ mic: null, cam: null, speaker: null, backgroundEffect: { kind: "zap" } }),
      ).backgroundEffect,
    ).toEqual(BACKGROUND_OFF);
  });

  test("setBackgroundEffectPref updates the device-prefs atom", () => {
    setBackgroundEffectPref({ kind: "blur" });
    expect($devicePrefs.get().backgroundEffect).toEqual({ kind: "blur" });
    setBackgroundEffectPref(BACKGROUND_OFF);
    expect($devicePrefs.get().backgroundEffect).toEqual(BACKGROUND_OFF);
  });
});
