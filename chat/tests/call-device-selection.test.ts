import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $devicePrefs,
  defaultAudioProcessingPrefs,
  type AudioProcessingPrefs,
} from "../src/lib/calls/device-prefs";
import {
  applyAiNoiseModelSelection,
  applyAudioProcessingSelection,
  applyBackgroundEffectSelection,
  applyCallDeviceSelection,
} from "../src/lib/calls/call-device-selection";
import { $aiNoiseFilterError } from "../src/lib/calls/ai-noise-filter-error-state";
import { $backgroundEffectError } from "../src/lib/calls/background-effect-error-state";
import { BACKGROUND_OFF } from "../src/lib/calls/background-effect/effect-id";
import {
  $callMediaIssues,
  clearAllMediaIssues,
} from "../src/lib/calls/call-media-issues";

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
  $devicePrefs.set({
    mic: null,
    cam: null,
    speaker: null,
    audioProcessing: defaultAudioProcessingPrefs(),
    aiNoiseModel: null,
    backgroundEffect: BACKGROUND_OFF,
  });
  $aiNoiseFilterError.set(null);
  $backgroundEffectError.set(null);
  clearAllMediaIssues();
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
});

describe("call device selection", () => {
  test("persists and applies mic, camera, and speaker selections to the active call", async () => {
    setEnumeratedDevices([
      { deviceId: "headset-mic", kind: "audioinput", label: "Headset" },
      { deviceId: "desk-cam", kind: "videoinput", label: "Desk cam" },
      { deviceId: "usb-speaker", kind: "audiooutput", label: "USB speaker" },
    ]);
    const engine = {
      setMicDevice: mock(async (_id: string) => undefined),
      setCameraDevice: mock(async (_id: string) => undefined),
      setSpeakerDevice: mock(async (_id: string) => undefined),
    };

    await applyCallDeviceSelection("mic", "headset-mic", engine);
    await applyCallDeviceSelection("cam", "desk-cam", engine);
    await applyCallDeviceSelection("speaker", "usb-speaker", engine);

    expect($devicePrefs.get()).toEqual({
      mic: "headset-mic",
      cam: "desk-cam",
      speaker: "usb-speaker",
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      backgroundEffect: BACKGROUND_OFF,
    });
    expect(engine.setMicDevice).toHaveBeenCalledWith("headset-mic");
    expect(engine.setCameraDevice).toHaveBeenCalledWith("desk-cam");
    expect(engine.setSpeakerDevice).toHaveBeenCalledWith("usb-speaker");
  });

  test("applies the system default device without leaving the active call", async () => {
    const engine = {
      setMicDevice: mock(async (_id: string) => undefined),
      setCameraDevice: mock(async (_id: string) => undefined),
      setSpeakerDevice: mock(async (_id: string) => undefined),
    };

    await applyCallDeviceSelection("mic", null, engine);
    await applyCallDeviceSelection("cam", null, engine);
    await applyCallDeviceSelection("speaker", null, engine);

    expect($devicePrefs.get()).toEqual({
      mic: null,
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      backgroundEffect: BACKGROUND_OFF,
    });
    expect(engine.setMicDevice).toHaveBeenCalledWith("default");
    expect(engine.setCameraDevice).toHaveBeenCalledWith("default");
    expect(engine.setSpeakerDevice).toHaveBeenCalledWith("default");
  });

  test("does not persist a preference when the active call rejects the device switch", async () => {
    setEnumeratedDevices([
      { deviceId: "fallback-mic", kind: "audioinput", label: "Fallback mic" },
    ]);
    const engine = {
      setMicDevice: mock(async (_id: string) => {
        throw new Error("device unavailable");
      }),
      setCameraDevice: mock(async (_id: string) => undefined),
      setSpeakerDevice: mock(async (_id: string) => undefined),
    };
    $devicePrefs.set({
      mic: "old-mic",
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      backgroundEffect: BACKGROUND_OFF,
    });

    await expect(applyCallDeviceSelection("mic", null, engine)).rejects.toThrow("device unavailable");

    expect(engine.setMicDevice).toHaveBeenCalledWith("default");
    expect($devicePrefs.get()).toEqual({
      mic: "old-mic",
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
      backgroundEffect: BACKGROUND_OFF,
    });
  });

  test("persists the browser default when the requested saved device id is missing", async () => {
    setEnumeratedDevices([
      { deviceId: "fallback-mic", kind: "audioinput", label: "Fallback mic" },
    ]);
    const engine = {
      setMicDevice: mock(async (_id: string) => undefined),
      setCameraDevice: mock(async (_id: string) => undefined),
      setSpeakerDevice: mock(async (_id: string) => undefined),
    };

    await applyCallDeviceSelection("mic", "stale-mic", engine);

    expect(engine.setMicDevice).toHaveBeenCalledWith("default");
    expect($devicePrefs.get().mic).toBeNull();
    expect($callMediaIssues.get().mic).toBe("missing");
  });

  test("persists and applies audio processing selections to the active call", async () => {
    const audioProcessing: AudioProcessingPrefs = {
      noiseSuppression: false,
      echoCancellation: false,
      autoGainControl: false,
    };
    const engine = {
      setAudioProcessing: mock(async (_prefs: AudioProcessingPrefs) => undefined),
    };

    await applyAudioProcessingSelection(audioProcessing, engine);

    expect($devicePrefs.get().audioProcessing).toEqual(audioProcessing);
    expect(engine.setAudioProcessing).toHaveBeenCalledWith(audioProcessing);
  });

  test("does not persist audio processing when the active call rejects restart", async () => {
    const engine = {
      setAudioProcessing: mock(async (_prefs: AudioProcessingPrefs) => {
        throw new Error("restart failed");
      }),
    };

    await expect(
      applyAudioProcessingSelection(
        {
          noiseSuppression: false,
          echoCancellation: false,
          autoGainControl: false,
        },
        engine,
      ),
    ).rejects.toThrow("restart failed");

    expect($devicePrefs.get().audioProcessing).toEqual(defaultAudioProcessingPrefs());
  });
});

describe("applyAiNoiseModelSelection", () => {
  test("applies the model to the engine and persists the pref", async () => {
    const setAiNoiseModel = mock(async (_m: string | null) => undefined);

    await applyAiNoiseModelSelection("rnnoise", { setAiNoiseModel });

    expect(setAiNoiseModel).toHaveBeenCalledWith("rnnoise");
    expect($devicePrefs.get().aiNoiseModel).toBe("rnnoise");
  });

  test("clears a stale attach-failure notice when re-selecting", async () => {
    $aiNoiseFilterError.set("dtln");
    const setAiNoiseModel = mock(async (_m: string | null) => undefined);

    await applyAiNoiseModelSelection("rnnoise", { setAiNoiseModel });

    expect($aiNoiseFilterError.get()).toBeNull();
  });

  test("turning the filter off persists null", async () => {
    const setAiNoiseModel = mock(async (_m: string | null) => undefined);

    await applyAiNoiseModelSelection(null, { setAiNoiseModel });

    expect(setAiNoiseModel).toHaveBeenCalledWith(null);
    expect($devicePrefs.get().aiNoiseModel).toBeNull();
  });
});

describe("applyBackgroundEffectSelection", () => {
  test("applies the effect to the engine and persists the pref", async () => {
    const effect = { kind: "image", image: { source: "catalog", id: "office" } } as const;
    const setBackgroundEffect = mock(async () => undefined);

    await applyBackgroundEffectSelection(effect, { setBackgroundEffect });

    expect(setBackgroundEffect).toHaveBeenCalledWith(effect);
    expect($devicePrefs.get().backgroundEffect).toEqual(effect);
  });

  test("clears a stale attach-failure notice when re-selecting", async () => {
    $backgroundEffectError.set({ kind: "blur" });
    const setBackgroundEffect = mock(async () => undefined);

    await applyBackgroundEffectSelection({ kind: "blur" }, { setBackgroundEffect });

    expect($backgroundEffectError.get()).toBeNull();
  });

  test("turning the effect off persists off", async () => {
    const setBackgroundEffect = mock(async () => undefined);

    await applyBackgroundEffectSelection(BACKGROUND_OFF, { setBackgroundEffect });

    expect(setBackgroundEffect).toHaveBeenCalledWith(BACKGROUND_OFF);
    expect($devicePrefs.get().backgroundEffect).toEqual(BACKGROUND_OFF);
  });
});
