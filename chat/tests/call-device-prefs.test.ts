import { describe, expect, test } from "bun:test";
import {
  audioProcessingConstraints,
  defaultAudioProcessingPrefs,
  isSpeakerOutputSelectionSupported,
  normalizeAudioProcessingPrefs,
  parseDevicePrefsStorage,
  serializeDevicePrefsStorage,
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
    });
  });
});
