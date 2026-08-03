import { afterEach, describe, expect, test } from "bun:test";
import {
  $callMediaIssues,
  classifyMediaError,
  clearAllMediaIssues,
  clearMediaIssue,
  mediaErrorMessage,
  mediaIssueMessage,
  recordMediaIssue,
} from "../src/lib/calls/call-media-issues";
import { __setFaroForTesting } from "../src/lib/telemetry";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import { $devicePrefs, setMicDevice } from "../src/lib/calls/device-prefs";
import { $callMicEnabled } from "../src/lib/calls/call-mic-state";

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");

function domError(name: string): Error {
  const err = new Error(`${name} message`);
  err.name = name;
  return err;
}

afterEach(() => {
  clearAllMediaIssues();
  __setFaroForTesting(null);
  const engine = useCallEngine().engine as unknown as {
    emit: (event: string, ...args: unknown[]) => void;
  };
  engine.emit("disconnected", { origin: "local" });
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
});

describe("classifyMediaError", () => {
  test("maps permission rejections (incl. Chrome legacy aliases) to 'denied'", () => {
    expect(classifyMediaError(domError("NotAllowedError"))).toBe("denied");
    expect(classifyMediaError(domError("PermissionDeniedError"))).toBe("denied");
    expect(classifyMediaError(domError("SecurityError"))).toBe("denied");
  });

  test("maps missing-device rejections to 'missing'", () => {
    expect(classifyMediaError(domError("NotFoundError"))).toBe("missing");
    expect(classifyMediaError(domError("DevicesNotFoundError"))).toBe("missing");
    expect(classifyMediaError(domError("OverconstrainedError"))).toBe("missing");
  });

  test("maps busy-device rejections to 'in-use'", () => {
    expect(classifyMediaError(domError("NotReadableError"))).toBe("in-use");
    expect(classifyMediaError(domError("TrackStartError"))).toBe("in-use");
    expect(classifyMediaError(domError("AbortError"))).toBe("in-use");
  });

  test("falls back to 'failed' for unknown / non-error values", () => {
    expect(classifyMediaError(domError("WeirdError"))).toBe("failed");
    expect(classifyMediaError(new Error("plain"))).toBe("failed");
    expect(classifyMediaError("nope")).toBe("failed");
    expect(classifyMediaError(null)).toBe("failed");
  });
});

describe("mediaErrorMessage (generic toast mapping)", () => {
  test("returns friendly copy for media DOMExceptions", () => {
    expect(mediaErrorMessage(domError("NotAllowedError"))).toContain("blocked");
    expect(mediaErrorMessage(domError("NotFoundError"))).toContain("No camera or microphone");
  });

  test("returns null for non-media errors so the caller keeps the raw message", () => {
    expect(mediaErrorMessage(new Error("xmpp timeout"))).toBeNull();
    expect(mediaErrorMessage("boom")).toBeNull();
  });
});

describe("mediaIssueMessage (notice copy)", () => {
  test("is kind-specific", () => {
    expect(mediaIssueMessage("mic", "missing")).toContain("microphone");
    expect(mediaIssueMessage("cam", "missing")).toContain("camera");
    expect(mediaIssueMessage("screen", "failed")).toContain("screen sharing");
    expect(mediaIssueMessage("mic", "denied")).toContain("listener");
  });
});

describe("$callMediaIssues store", () => {
  test("reports permission and missing-device failures as recoverable Faro errors", () => {
    const errors: Array<{
      error: Error;
      options: { type?: string; context?: Record<string, string> };
    }> = [];
    __setFaroForTesting({
      api: {
        pushError: (error: Error, options?: { type?: string; context?: Record<string, string> }) => {
          errors.push({ error, options: options ?? {} });
        },
      },
    } as never);

    recordMediaIssue("mic", domError("NotAllowedError"));
    recordMediaIssue("cam", domError("NotFoundError"));

    expect(errors.map(({ options }) => options)).toEqual([
      {
        type: "call.media",
        context: expect.objectContaining({
          kind: "call.media",
          recoverable: "true",
          media_kind: "mic",
          reason: "denied",
        }),
      },
      {
        type: "call.media",
        context: expect.objectContaining({
          kind: "call.media",
          recoverable: "true",
          media_kind: "cam",
          reason: "missing",
        }),
      },
    ]);
    expect(errors.map(({ error }) => error.message)).toEqual([
      "call.media.mic.denied",
      "call.media.cam.missing",
    ]);
    expect(errors.map(({ error }) => error.message).join(" ")).not.toContain("NotAllowedError message");
  });

  test("record/clear a single kind without touching the other", () => {
    recordMediaIssue("mic", domError("NotFoundError"));
    expect($callMediaIssues.get()).toEqual({ mic: "missing", cam: null, screen: null });

    recordMediaIssue("cam", domError("NotAllowedError"));
    expect($callMediaIssues.get()).toEqual({ mic: "missing", cam: "denied", screen: null });

    recordMediaIssue("screen", domError("NotReadableError"));
    expect($callMediaIssues.get()).toEqual({
      mic: "missing",
      cam: "denied",
      screen: "in-use",
    });

    clearMediaIssue("mic");
    expect($callMediaIssues.get()).toEqual({ mic: null, cam: "denied", screen: "in-use" });
  });

  test("clearAllMediaIssues resets every kind", () => {
    recordMediaIssue("mic", domError("NotReadableError"));
    recordMediaIssue("cam", domError("NotFoundError"));
    recordMediaIssue("screen", domError("AbortError"));
    clearAllMediaIssues();
    expect($callMediaIssues.get()).toEqual({ mic: null, cam: null, screen: null });
  });

  test("the engine mediaDevicesError event records a mid-call issue", () => {
    const engine = useCallEngine().engine as unknown as {
      emit: (event: string, ...args: unknown[]) => void;
    };

    engine.emit("mediaDevicesError", {
      source: "audio",
      error: domError("NotFoundError"),
    });

    expect($callMediaIssues.get()).toEqual({ mic: "missing", cam: null, screen: null });
  });

  test("an engine media error without a device kind surfaces as a screen issue", () => {
    const engine = useCallEngine().engine as unknown as {
      emit: (event: string, ...args: unknown[]) => void;
    };

    // livekit maps ScreenShare/Unknown sources to an undefined kind; the
    // engine routes those to source "screen" instead of dropping them.
    engine.emit("mediaDevicesError", {
      source: "screen",
      error: domError("NotReadableError"),
    });

    expect($callMediaIssues.get()).toEqual({ mic: null, cam: null, screen: "in-use" });
  });

  test("a failed unplug fallback records the issue and turns the mic toggle off", async () => {
    let onDeviceChange: (() => void) | null = null;
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          enumerateDevices: async () => [],
          addEventListener: (_event: string, handler: () => void) => {
            onDeviceChange = handler;
          },
          removeEventListener: () => undefined,
        },
      },
    });
    const engine = useCallEngine().engine as unknown as {
      activeDeviceId: (kind: MediaDeviceKind) => string | null;
      emit: (event: string, ...args: unknown[]) => void;
      setMicDevice: (deviceId: string) => Promise<void>;
    };
    const originalActiveDeviceId = engine.activeDeviceId;
    const originalSetMicDevice = engine.setMicDevice;
    $callMicEnabled.set(true);
    try {
      engine.activeDeviceId = (kind: MediaDeviceKind) =>
        kind === "audioinput" ? "gone-mic" : null;
      engine.setMicDevice = async () => {
        throw domError("NotFoundError");
      };

      engine.emit("connected", {
        localIdentity: "alice@waddle.test/web",
        remoteIdentities: [],
        roomName: "room@muc.waddle.test::call",
      });
      onDeviceChange?.();
      await new Promise((resolve) => setTimeout(resolve, 250));

      expect($callMediaIssues.get().mic).toBe("missing");
      // Capture is confirmed lost: the toggle must read OFF so the
      // notice's "Enable mic" action requests a re-enable, not a
      // disable of a stale on-state.
      expect($callMicEnabled.get()).toBe(false);

      engine.emit("disconnected", { origin: "local" });
    } finally {
      $callMicEnabled.set(true);
      engine.activeDeviceId = originalActiveDeviceId;
      engine.setMicDevice = originalSetMicDevice;
    }
  });

  test("devicechange falling out from under the active mic falls back without a false notice", async () => {
    let onDeviceChange: (() => void) | null = null;
    let removedHandler: (() => void) | null = null;
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          enumerateDevices: async () => [
            { deviceId: "replacement-mic", kind: "audioinput", label: "Replacement" },
          ],
          addEventListener: (_event: string, handler: () => void) => {
            onDeviceChange = handler;
          },
          removeEventListener: (_event: string, handler: () => void) => {
            removedHandler = handler;
          },
        },
      },
    });

    const engine = useCallEngine().engine as unknown as {
      activeDeviceId: (kind: MediaDeviceKind) => string | null;
      emit: (event: string, ...args: unknown[]) => void;
      setMicDevice: (deviceId: string) => Promise<void>;
      setCameraDevice: (deviceId: string) => Promise<void>;
      setSpeakerDevice: (deviceId: string) => Promise<void>;
    };
    const originalActiveDeviceId = engine.activeDeviceId;
    const originalSetMicDevice = engine.setMicDevice;
    const originalSetCameraDevice = engine.setCameraDevice;
    const originalSetSpeakerDevice = engine.setSpeakerDevice;
    const micCalls: string[] = [];
    let activeMicId = "gone-mic";
    setMicDevice("gone-mic");
    try {
      engine.activeDeviceId = (kind: MediaDeviceKind) =>
        kind === "audioinput" ? activeMicId : null;
      engine.setMicDevice = async (deviceId: string) => {
        micCalls.push(deviceId);
      };
      engine.setCameraDevice = async () => undefined;
      engine.setSpeakerDevice = async () => undefined;

      engine.emit("connected", {
        localIdentity: "alice@waddle.test/web",
        remoteIdentities: [],
        roomName: "room@muc.waddle.test::call",
      });
      onDeviceChange?.();
      // LiveKit's own listener auto-selects the first remaining device before
      // our debounced reconciliation runs. We must still remember that the
      // device active at event time was the one removed.
      activeMicId = "replacement-mic";
      await new Promise((resolve) => setTimeout(resolve, 250));

      // The fallback SUCCEEDED: capture is live on the default, so no
      // "missing" notice is recorded (its enable action would disable
      // the working device — #1621 round 4), and the SAVED preference
      // survives so replugging restores the user's choice.
      expect($callMediaIssues.get()).toEqual({ mic: null, cam: null, screen: null });
      expect(micCalls).toEqual(["default"]);
      expect($devicePrefs.get().mic).toBe("gone-mic");

      engine.emit("disconnected", { origin: "local" });
      expect(removedHandler).toBe(onDeviceChange);
    } finally {
      setMicDevice(null);
      engine.activeDeviceId = originalActiveDeviceId;
      engine.setMicDevice = originalSetMicDevice;
      engine.setCameraDevice = originalSetCameraDevice;
      engine.setSpeakerDevice = originalSetSpeakerDevice;
    }
  });
});
