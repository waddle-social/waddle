import { afterEach, describe, expect, mock, test } from "bun:test";
import { readFileSync } from "node:fs";
import { $callState, clearCallState } from "../src/lib/calls/call-store";
import {
  $callCamEnabled,
  $callScreenShareEnabled,
  $callScreenShareSupported,
  $callMicEnabled,
  installCallPagehideSuspension,
  refreshScreenShareSupported,
  resetCallControls,
  seedCallControlsFromEngine,
  suspendCallForPageHide,
  toggleScreenShare,
  toggleMic,
} from "../src/lib/calls/call-controls";
import {
  $callMediaIssues,
  clearAllMediaIssues,
  recordMediaIssue,
} from "../src/lib/calls/call-media-issues";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import {
  $dmCallActivities,
  clearDmCallActivities,
} from "../src/lib/calls/dm-call-activity";
import {
  $mucCallLiveParticipants,
  setLiveCallParticipants,
} from "../src/lib/calls/muc-call-live-participants";
import { connectionStore } from "../src/lib/connection-store";
import type { LiveKitJoin } from "../src/lib/calls/types";
import { Track } from "livekit-client";

const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c1",
  identity: "alice@waddle.test/web",
  token: "jwt",
};

class PagehideHarness {
  private readonly listeners = new Set<EventListener>();

  addEventListener(type: string, listener: EventListener) {
    if (type === "pagehide") this.listeners.add(listener);
  }

  removeEventListener(type: string, listener: EventListener) {
    if (type === "pagehide") this.listeners.delete(listener);
  }

  dispatchPagehide(persisted: boolean) {
    const event = new Event("pagehide") as PageTransitionEvent;
    Object.defineProperty(event, "persisted", { value: persisted });
    for (const listener of this.listeners) listener(event);
  }

  listenerCount(): number {
    return this.listeners.size;
  }
}

function deviceError(name: string): Error {
  const err = new Error(`${name} message`);
  err.name = name;
  return err;
}

afterEach(() => {
  clearCallState();
  clearDmCallActivities();
  $mucCallLiveParticipants.set({});
  connectionStore.client = null;
  clearAllMediaIssues();
  $callScreenShareEnabled.set(false);
  $callScreenShareSupported.set(false);
  // The engine is a process-wide singleton; drop any injected room
  // stub so it doesn't leak into the next test.
  (useCallEngine().engine as unknown as { room: unknown }).room = null;
});

describe("screenshare support detection", () => {
  test("starts disabled for SSR-safe hydration and refreshes from browser capability", () => {
    expect($callScreenShareSupported.get()).toBe(false);
    const original = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: {
        mediaDevices: {
          getDisplayMedia: () => undefined,
        },
      },
    });
    try {
      refreshScreenShareSupported();
      expect($callScreenShareSupported.get()).toBe(true);
    } finally {
      if (original) Object.defineProperty(globalThis, "navigator", original);
      else Reflect.deleteProperty(globalThis, "navigator");
    }
  });
});

describe("call page lifecycle controls", () => {
  test("pagehide suspension clears only local media state, preserving rediscoverable DM activity", () => {
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });
    $dmCallActivities.set({
      "bob@waddle.test": {
        peerJid: "bob@waddle.test",
        sid: "c1",
        media: { audio: true, video: true },
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-26T00:00:00.000Z",
      },
    });

    suspendCallForPageHide();

    expect($callState.get()).toEqual({ phase: "idle" });
    expect($dmCallActivities.get()["bob@waddle.test"]).toMatchObject({
      sid: "c1",
      state: "accepted",
    });
  });

  test("pagehide suspension does not emit XMPP call-ending stanzas", () => {
    const sender = {
      send_call_session_terminate: mock(async () => undefined),
      send_call_finish: mock(async () => undefined),
      update_muji_presence: mock(async () => undefined),
      send_raw_iq: mock(async () => "<iq type='result'/>"),
    };
    connectionStore.client = {
      xmpp: sender,
    } as unknown as typeof connectionStore.client;
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid: "muc-1",
      media: { audio: true, video: false },
      join: { ...join, room: "room@muc.waddle.test" },
      kind: "muc",
      selfNick: "alice",
      selfFullJid: "alice@waddle.test/web",
    });

    suspendCallForPageHide();

    expect(sender.send_call_session_terminate).not.toHaveBeenCalled();
    expect(sender.send_call_finish).not.toHaveBeenCalled();
    expect(sender.update_muji_presence).not.toHaveBeenCalled();
    expect(sender.send_raw_iq).not.toHaveBeenCalled();
  });

  test("pagehide suspension clears stale LiveKit participant projection for MUC calls", () => {
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid: "muc-1",
      media: { audio: true, video: false },
      join: { ...join, room: "room@muc.waddle.test" },
      kind: "muc",
      selfNick: "alice",
      selfFullJid: "alice@waddle.test/web",
    });
    setLiveCallParticipants("room@muc.waddle.test", [
      "alice@waddle.test/web",
      "bob@waddle.test/phone",
    ]);

    suspendCallForPageHide();

    expect($mucCallLiveParticipants.get()).toEqual({});
  });

  test("pagehide suspension persists stream resume state before clearing the local call slot", () => {
    const persistResumeStateForPageHide = mock(() => undefined);
    connectionStore.client = {
      persistResumeStateForPageHide,
    } as unknown as typeof connectionStore.client;
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });

    suspendCallForPageHide();

    expect(persistResumeStateForPageHide).toHaveBeenCalledTimes(1);
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("pagehide persists stream resume state for rediscovered idle DM calls", () => {
    const persistResumeStateForPageHide = mock(() => undefined);
    connectionStore.client = {
      persistResumeStateForPageHide,
    } as unknown as typeof connectionStore.client;
    $callState.set({ phase: "idle" });
    $dmCallActivities.set({
      "bob@waddle.test": {
        peerJid: "bob@waddle.test",
        remoteFullJid: "bob@waddle.test/desktop",
        sid: "c1",
        media: { audio: true, video: true },
        join,
        state: "accepted",
        direction: "incoming",
        updatedAt: "2026-05-26T00:00:00.000Z",
      },
    });

    suspendCallForPageHide();

    expect(persistResumeStateForPageHide).toHaveBeenCalledTimes(1);
    expect($callState.get()).toEqual({ phase: "idle" });
    expect($dmCallActivities.get()["bob@waddle.test"]?.sid).toBe("c1");
  });

  test("pagehide listener ignores BFCache restores and suspends refresh unloads", () => {
    const target = new PagehideHarness();
    const persistResumeStateForPageHide = mock(() => undefined);
    connectionStore.client = {
      persistResumeStateForPageHide,
    } as unknown as typeof connectionStore.client;
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });

    const disconnect = installCallPagehideSuspension(target as unknown as Window);
    expect(target.listenerCount()).toBe(1);

    target.dispatchPagehide(true);
    expect(persistResumeStateForPageHide).not.toHaveBeenCalled();
    expect($callState.get()).toMatchObject({ phase: "active", sid: "c1" });

    target.dispatchPagehide(false);
    expect(persistResumeStateForPageHide).toHaveBeenCalledTimes(1);
    expect($callState.get()).toEqual({ phase: "idle" });

    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c2",
      media: { audio: true, video: false },
      join,
      kind: "dm",
    });
    disconnect();
    expect(target.listenerCount()).toBe(0);
    target.dispatchPagehide(false);
    expect(persistResumeStateForPageHide).toHaveBeenCalledTimes(1);
    expect($callState.get()).toMatchObject({ phase: "active", sid: "c2" });
  });

  test("CallOverlay wires the browser pagehide event to suspension, not hangup", () => {
    const source = readFileSync(new URL("../src/components/calls/CallOverlay.vue", import.meta.url), "utf8");
    const unmountBlock = source.slice(source.indexOf("onBeforeUnmount(() => {"));

    expect(source).toContain("installCallPagehideSuspension(window)");
    expect(unmountBlock).toContain("void engine.disconnect();");
    expect(unmountBlock).not.toContain("tearDownActiveCall(");
  });
});

describe("device-less call controls", () => {
  test("seedCallControlsFromEngine reflects ACTUAL published state, not the request", () => {
    // Optimistic baseline says both on; the engine reports the camera
    // never published (no device / denied). Seeding must trust the engine.
    $callMicEnabled.set(true);
    $callCamEnabled.set(true);
    seedCallControlsFromEngine({ micEnabled: true, cameraEnabled: false });
    expect($callMicEnabled.get()).toBe(true);
    expect($callCamEnabled.get()).toBe(false);
    expect($callScreenShareEnabled.get()).toBe(false);
  });

  test("resetCallControls clears media issues left over from a prior call", () => {
    recordMediaIssue("mic", deviceError("NotFoundError"));
    recordMediaIssue("cam", deviceError("NotAllowedError"));
    recordMediaIssue("screen", deviceError("AbortError"));
    resetCallControls(true, true);
    expect($callMediaIssues.get()).toEqual({ mic: null, cam: null, screen: null });
    expect($callMicEnabled.get()).toBe(true);
    expect($callCamEnabled.get()).toBe(true);
    expect($callScreenShareEnabled.get()).toBe(false);
  });

  test("toggleMic retry that is still denied rolls back AND records a classified issue", async () => {
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setMicrophoneEnabled: async () => {
          throw deviceError("NotAllowedError");
        },
      },
    };
    // User is muted (device-less) and clicks "enable mic"; permission
    // is still blocked, so the atom must roll back to muted and the
    // notice must update — not a transient error toast.
    $callMicEnabled.set(false);
    await toggleMic();
    expect($callMicEnabled.get()).toBe(false);
    expect($callMediaIssues.get().mic).toBe("denied");
  });

  test("toggleMic retry that succeeds clears the recorded issue", async () => {
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setMicrophoneEnabled: async () => undefined,
      },
    };
    recordMediaIssue("mic", deviceError("NotFoundError"));
    $callMicEnabled.set(false);
    await toggleMic();
    expect($callMicEnabled.get()).toBe(true);
    expect($callMediaIssues.get().mic).toBeNull();
  });

  test("toggleScreenShare publishes screen video with optimistic state", async () => {
    const { engine } = useCallEngine();
    const calls: Array<{ enabled: boolean; audio: boolean }> = [];
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setScreenShareEnabled: async (enabled: boolean, options?: { audio?: boolean }) => {
          calls.push({ enabled, audio: options?.audio ?? false });
        },
      },
    };
    $callScreenShareEnabled.set(false);
    await toggleScreenShare();
    expect(calls).toEqual([{ enabled: true, audio: false }]);
    expect($callScreenShareEnabled.get()).toBe(true);
    expect($callMediaIssues.get().screen).toBeNull();
  });

  test("toggleScreenShare successful retry clears a prior screen notice", async () => {
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setScreenShareEnabled: async () => undefined,
      },
    };
    recordMediaIssue("screen", deviceError("NotReadableError"));
    $callScreenShareEnabled.set(false);
    await toggleScreenShare();
    expect($callScreenShareEnabled.get()).toBe(true);
    expect($callMediaIssues.get().screen).toBeNull();
  });

  test("toggleScreenShare stop clears a prior screen notice", async () => {
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setScreenShareEnabled: async () => undefined,
      },
    };
    recordMediaIssue("screen", deviceError("NotReadableError"));
    $callScreenShareEnabled.set(true);
    await toggleScreenShare();
    expect($callScreenShareEnabled.get()).toBe(false);
    expect($callMediaIssues.get().screen).toBeNull();
  });

  test("toggleScreenShare picker cancellation rolls back silently", async () => {
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setScreenShareEnabled: async () => {
          throw deviceError("NotAllowedError");
        },
      },
    };
    $callScreenShareEnabled.set(false);
    await toggleScreenShare();
    expect($callScreenShareEnabled.get()).toBe(false);
    expect($callMediaIssues.get().screen).toBeNull();
  });

  test("toggleScreenShare genuine capture failure rolls back and records a notice", async () => {
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setScreenShareEnabled: async () => {
          throw deviceError("NotReadableError");
        },
      },
    };
    $callScreenShareEnabled.set(false);
    await toggleScreenShare();
    expect($callScreenShareEnabled.get()).toBe(false);
    expect($callMediaIssues.get().screen).toBe("in-use");
  });

  test("native browser stop syncs the screenshare toggle off through local track unpublish", () => {
    const { engine } = useCallEngine();
    $callScreenShareEnabled.set(true);
    recordMediaIssue("screen", deviceError("NotReadableError"));
    (
      engine as unknown as {
        handleLocalTrackUnpublished: (publication: unknown, participant: unknown) => void;
      }
    ).handleLocalTrackUnpublished(
      {
        track: {} as unknown,
        kind: Track.Kind.Video,
        source: Track.Source.ScreenShare,
        trackSid: "screen-pub",
      },
      { identity: "alice@waddle.test/web" },
    );
    expect($callScreenShareEnabled.get()).toBe(false);
    expect($callMediaIssues.get().screen).toBeNull();
  });
});
