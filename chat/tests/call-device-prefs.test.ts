import { afterEach, describe, expect, test } from "bun:test";
import {
  $devicePrefs,
  audioProcessingConstraints,
  defaultAudioProcessingPrefs,
  hasEnumeratedCallDeviceId,
  isSpeakerOutputSelectionSupported,
  normalizeAudioProcessingPrefs,
  parseDevicePrefsStorage,
  resolveCallDevicePreference,
  serializeDevicePrefsStorage,
  setAiNoiseModel,
  setBackgroundEffectPref,
} from "../src/lib/calls/device-prefs";
import { BACKGROUND_OFF } from "../src/lib/calls/background-effect/effect-id";

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");

function setEnumeratedDevices(
  devices: Array<{ deviceId: string; kind: MediaDeviceKind; label: string }>,
): void {
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: {
      mediaDevices: {
        enumerateDevices: async () => devices,
      },
    },
  });
}

afterEach(() => {
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
});

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

describe("call device preference resolution", () => {
  test("detects an enumerated device id for its picker kind", () => {
    const devices = {
      mics: [{ deviceId: "headset-mic", kind: "audioinput", label: "Headset" }],
      cams: [{ deviceId: "desk-cam", kind: "videoinput", label: "Desk cam" }],
      speakers: [{ deviceId: "usb-speaker", kind: "audiooutput", label: "USB speaker" }],
    };

    expect(hasEnumeratedCallDeviceId(devices, "mic", "headset-mic")).toBe(true);
    expect(hasEnumeratedCallDeviceId(devices, "cam", "headset-mic")).toBe(false);
  });

  test("keeps an available saved device id for capture and active switching", async () => {
    setEnumeratedDevices([
      { deviceId: "headset-mic", kind: "audioinput", label: "Headset" },
      { deviceId: "desk-cam", kind: "videoinput", label: "Desk cam" },
    ]);

    await expect(resolveCallDevicePreference("mic", "headset-mic")).resolves.toEqual({
      activeDeviceId: "headset-mic",
      preferenceId: "headset-mic",
      captureDeviceId: "headset-mic",
      missing: false,
    });
  });

  test("enumeration failure resolves a saved device to defaults without failing the join", async () => {
    // This runs BEFORE the Room is constructed: a rejecting
    // enumerateDevices() must degrade to browser defaults (no `missing`
    // notice — best-effort capture surfaces any real device error), never
    // propagate and turn a saved preference into a fatal join failure.
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          enumerateDevices: async () => {
            throw new Error("enumeration unavailable");
          },
        },
      },
    });

    await expect(resolveCallDevicePreference("mic", "headset-mic")).resolves.toEqual({
      activeDeviceId: "default",
      preferenceId: null,
      captureDeviceId: undefined,
      missing: false,
    });
  });

  test("falls back to the browser default when a saved device id is gone", async () => {
    setEnumeratedDevices([
      { deviceId: "desk-cam", kind: "videoinput", label: "Desk cam" },
    ]);

    await expect(resolveCallDevicePreference("mic", "stale-mic")).resolves.toEqual({
      activeDeviceId: "default",
      preferenceId: null,
      captureDeviceId: undefined,
      missing: true,
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
