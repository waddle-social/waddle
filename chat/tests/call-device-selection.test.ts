import { afterEach, describe, expect, mock, test } from "bun:test";
import {
  $devicePrefs,
  defaultAudioProcessingPrefs,
  type AudioProcessingPrefs,
} from "../src/lib/calls/device-prefs";
import {
  applyAiNoiseModelSelection,
  applyAudioProcessingSelection,
  applyCallDeviceSelection,
} from "../src/lib/calls/call-device-selection";
import { $aiNoiseFilterError } from "../src/lib/calls/ai-noise-filter-error-state";

afterEach(() => {
  $devicePrefs.set({
    mic: null,
    cam: null,
    speaker: null,
    audioProcessing: defaultAudioProcessingPrefs(),
    aiNoiseModel: null,
  });
  $aiNoiseFilterError.set(null);
});

describe("call device selection", () => {
  test("persists and applies mic, camera, and speaker selections to the active call", async () => {
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
    });
    expect(engine.setMicDevice).toHaveBeenCalledWith("default");
    expect(engine.setCameraDevice).toHaveBeenCalledWith("default");
    expect(engine.setSpeakerDevice).toHaveBeenCalledWith("default");
  });

  test("does not persist a preference when the active call rejects the device switch", async () => {
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
    });

    await expect(applyCallDeviceSelection("mic", null, engine)).rejects.toThrow("device unavailable");

    expect(engine.setMicDevice).toHaveBeenCalledWith("default");
    expect($devicePrefs.get()).toEqual({
      mic: "old-mic",
      cam: null,
      speaker: null,
      audioProcessing: defaultAudioProcessingPrefs(),
      aiNoiseModel: null,
    });
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
