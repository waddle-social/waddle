import { afterEach, describe, expect, mock, test } from "bun:test";
import { $devicePrefs } from "../src/lib/calls/device-prefs";
import { applyCallDeviceSelection } from "../src/lib/calls/call-device-selection";

afterEach(() => {
  $devicePrefs.set({ mic: null, cam: null, speaker: null });
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

    expect($devicePrefs.get()).toEqual({ mic: null, cam: null, speaker: null });
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
    $devicePrefs.set({ mic: "old-mic", cam: null, speaker: null });

    await expect(applyCallDeviceSelection("mic", null, engine)).rejects.toThrow("device unavailable");

    expect(engine.setMicDevice).toHaveBeenCalledWith("default");
    expect($devicePrefs.get()).toEqual({ mic: "old-mic", cam: null, speaker: null });
  });
});
