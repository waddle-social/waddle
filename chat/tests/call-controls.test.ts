import { afterEach, describe, expect, mock, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join as joinPath } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { createSSRApp, h, ref } from "vue";
import { renderToString } from "vue/server-renderer";
import { parse, compileScript, compileTemplate } from "vue/compiler-sfc";
import ts from "typescript";
import { $callState, clearCallState } from "../src/lib/calls/call-store";
import {
  $callCamEnabled,
  $callScreenShareEnabled,
  $callScreenShareSupported,
  $callMicEnabled,
  hangupActiveCall,
  refreshScreenShareSupported,
  resetCallControls,
  seedCallControlsFromEngine,
  setPushToTalkActive,
  suspendCallForPageHide,
  toggleScreenShare,
  toggleMic,
} from "../src/lib/calls/call-controls";
import {
  $callMediaIssues,
  clearAllMediaIssues,
  recordMediaIssue,
} from "../src/lib/calls/call-media-issues";
import {
  $callAudioPlaybackBlocked,
  resumeCallAudioPlayback,
} from "../src/lib/calls/call-audio-playback";
import { CallAudioResumeAttemptGuard } from "../src/lib/calls/call-audio-resume-attempt";
import { useCallAudioPlaybackPromptController } from "../src/lib/calls/call-audio-playback-prompt-controller";
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
import { renderVueComponent as renderSfcComponent } from "./helpers/render-vue-sfc";
import { $callUiMode } from "../src/lib/calls/ui-mode";
import {
  __resetCallLifecycleTelemetryForTesting,
  beginCallAttempt,
  finishCallAttempt,
  markCallAttemptAccepted,
} from "../src/lib/calls/call-lifecycle-telemetry";
import { __setFaroForTesting } from "../src/lib/telemetry";

const require = createRequire(import.meta.url);

const join: LiveKitJoin = {
  url: "wss://livekit.test",
  room: "alice@waddle.test::c1",
  identity: "alice@waddle.test/web",
  token: "jwt",
};

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
  connectionStore.appState = "loading";
  clearAllMediaIssues();
  $callAudioPlaybackBlocked.set(false);
  $callScreenShareEnabled.set(false);
  $callScreenShareSupported.set(false);
  $callUiMode.set("split");
  // The engine is a process-wide singleton; drop any injected room
  // stub so it doesn't leak into the next test.
  (useCallEngine().engine as unknown as { room: unknown }).room = null;
  __resetCallLifecycleTelemetryForTesting();
  __setFaroForTesting(null);
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
  test("pagehide finalizes this attempt and rediscovery owns a separate lifecycle event", () => {
    const events: Array<{ name: string }> = [];
    __setFaroForTesting({
      api: { pushEvent: (name: string) => events.push({ name }) },
    } as never);
    beginCallAttempt("refresh-sid", "dm");
    markCallAttemptAccepted("refresh-sid", 1_000);
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "refresh-sid",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });

    suspendCallForPageHide();
    expect(events).toEqual([{ name: "chat.call.lifecycle" }]);

    __resetCallLifecycleTelemetryForTesting();
    beginCallAttempt("refresh-sid", "dm");
    markCallAttemptAccepted("refresh-sid", 2_000);
    finishCallAttempt("refresh-sid", { endReason: "hangup" }, 3_000);
    expect(events).toEqual([
      { name: "chat.call.lifecycle" },
      { name: "chat.call.lifecycle" },
    ]);
  });

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

  test("pagehide suspension leaves stream resume persistence to the shared XMPP lifecycle", () => {
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

    expect(persistResumeStateForPageHide).not.toHaveBeenCalled();
    expect($callState.get()).toEqual({ phase: "idle" });
  });

  test("XmppProvider owns browser pagehide suspension while CallOverlay remains a local lifecycle host", () => {
    const provider = readFileSync(new URL("../src/components/XmppProvider.vue", import.meta.url), "utf8");
    const source = readFileSync(new URL("../src/components/calls/CallOverlay.vue", import.meta.url), "utf8");
    const unmountBlock = source.slice(source.indexOf("onBeforeUnmount(() => {"));

    expect(provider).toContain("installXmppPagehideLifecycle(");
    expect(provider).toContain("suspendCallForPageHide,");
    expect(unmountBlock).toContain("void engine.disconnect();");
    expect(unmountBlock).not.toContain("tearDownActiveCall(");
  });
});

describe("hangupActiveCall media-first ordering (#1446)", () => {
  test("releases media and goes idle before any XMPP teardown send, even when the server never replies", async () => {
    const events: string[] = [];
    const sender = {
      send_call_session_terminate: mock(() => {
        events.push("terminate");
        return new Promise<void>(() => undefined); // server never replies
      }),
    };
    connectionStore.client = { xmpp: sender } as unknown as typeof connectionStore.client;
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      off: () => undefined,
      localParticipant: undefined,
      disconnect: async () => {
        events.push("disconnect");
      },
    };
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });

    await hangupActiveCall();

    // Camera/mic/LiveKit are released and the call slot is idle without
    // waiting on the terminate IQ — the server never replied at all.
    expect(events[0]).toBe("disconnect");
    expect($callState.get()).toEqual({ phase: "idle" });

    // The XEP-0166 terminate still goes out afterwards, in the background.
    for (let i = 0; i < 8; i += 1) await Promise.resolve();
    expect(events).toContain("terminate");
    expect(events.indexOf("terminate")).toBeGreaterThan(events.indexOf("disconnect"));
  });

  test("a stalled LiveKit disconnect cannot pin the call slot or delay the terminate", async () => {
    const sender = {
      send_call_session_terminate: mock(async () => undefined),
    };
    connectionStore.client = { xmpp: sender } as unknown as typeof connectionStore.client;
    const { engine } = useCallEngine();
    (engine as unknown as { room: unknown }).room = {
      off: () => undefined,
      localParticipant: undefined,
      disconnect: () => new Promise<void>(() => undefined), // dead network
    };
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid: "c1",
      media: { audio: true, video: true },
      join,
      kind: "dm",
    });

    void hangupActiveCall();

    // The slot flips idle synchronously, and the terminate goes out,
    // even though the engine disconnect never settles.
    expect($callState.get()).toEqual({ phase: "idle" });
    for (let i = 0; i < 8; i += 1) await Promise.resolve();
    expect(sender.send_call_session_terminate).toHaveBeenCalledTimes(1);
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

  test("push-to-talk activation unmutes a muted mic", async () => {
    const { engine } = useCallEngine();
    const calls: boolean[] = [];
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setMicrophoneEnabled: async (on: boolean) => {
          calls.push(on);
        },
      },
    };
    $callMicEnabled.set(false);
    setPushToTalkActive(true);
    await Promise.resolve();
    expect($callMicEnabled.get()).toBe(true);
    expect(calls).toEqual([true]);
  });

  test("push-to-talk release mutes a live mic", async () => {
    const { engine } = useCallEngine();
    const calls: boolean[] = [];
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setMicrophoneEnabled: async (on: boolean) => {
          calls.push(on);
        },
      },
    };
    $callMicEnabled.set(true);
    setPushToTalkActive(false);
    await Promise.resolve();
    expect($callMicEnabled.get()).toBe(false);
    expect(calls).toEqual([false]);
  });

  test("rapid push-to-talk tap settles on the released state without stranding the engine", async () => {
    // Press + release in the same tick. The enable must never win late and
    // re-open the mic — serialized last-writer-wins means only the disable
    // reaches the engine and the final state is muted.
    const { engine } = useCallEngine();
    const calls: boolean[] = [];
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setMicrophoneEnabled: async (on: boolean) => {
          calls.push(on);
        },
      },
    };
    $callMicEnabled.set(false);
    setPushToTalkActive(true);
    setPushToTalkActive(false);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect($callMicEnabled.get()).toBe(false);
    expect(calls).toEqual([false]);
  });

  test("push-to-talk activation is a no-op when the mic is already live", async () => {
    const { engine } = useCallEngine();
    const calls: boolean[] = [];
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setMicrophoneEnabled: async (on: boolean) => {
          calls.push(on);
        },
      },
    };
    $callMicEnabled.set(true);
    setPushToTalkActive(true);
    await Promise.resolve();
    expect($callMicEnabled.get()).toBe(true);
    expect(calls).toEqual([]);
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

  test("toggleScreenShare requests screen audio with optimistic state", async () => {
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
    expect(calls).toEqual([{ enabled: true, audio: true }]);
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
    const calls: Array<{ enabled: boolean; audio: boolean }> = [];
    (engine as unknown as { room: unknown }).room = {
      localParticipant: {
        setScreenShareEnabled: async (enabled: boolean, options?: { audio?: boolean }) => {
          calls.push({ enabled, audio: options?.audio ?? false });
        },
      },
    };
    recordMediaIssue("screen", deviceError("NotReadableError"));
    $callScreenShareEnabled.set(true);
    await toggleScreenShare();
    expect(calls).toEqual([{ enabled: false, audio: false }]);
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

  test("LiveKit audio playback status drives the tap-to-enable prompt state", () => {
    const { engine } = useCallEngine();
    (
      engine as unknown as {
        handleAudioPlaybackStatusChanged: (canPlaybackAudio: boolean) => void;
      }
    ).handleAudioPlaybackStatusChanged(false);
    expect($callAudioPlaybackBlocked.get()).toBe(true);

    (
      engine as unknown as {
        handleAudioPlaybackStatusChanged: (canPlaybackAudio: boolean) => void;
      }
    ).handleAudioPlaybackStatusChanged(true);
    expect($callAudioPlaybackBlocked.get()).toBe(false);
  });

  test("call audio playback prompt renders only while playback is blocked", async () => {
    const component = await loadVueComponent("../src/components/calls/CallAudioPlaybackPrompt.vue");

    $callAudioPlaybackBlocked.set(false);
    setActiveMucCall();
    expect(await renderVueComponent(component)).not.toContain("Tap to enable audio");

    $callAudioPlaybackBlocked.set(true);
    const html = await renderVueComponent(component);
    expect(html).toContain("Audio is paused by your browser.");
    expect(html).toContain("Tap to enable audio");
  });

  test("call audio playback prompt stays hidden outside an active call", async () => {
    const component = await loadVueComponent("../src/components/calls/CallAudioPlaybackPrompt.vue");

    $callAudioPlaybackBlocked.set(true);
    expect(await renderVueComponent(component)).not.toContain("Tap to enable audio");
  });

  test("app-level blocked audio recovery renders while viewing another conversation", async () => {
    const component = await loadVueComponent("../src/components/calls/CallAudioPlaybackPrompt.vue");

    setActiveMucCall();
    $callAudioPlaybackBlocked.set(true);

    const html = await renderToString(createSSRApp({
      render: () =>
        h("main", [
          h(component as never),
          h("section", { "aria-label": "Direct message with Bob" }, "Different conversation"),
        ]),
    }));

    expect(html).toContain("Audio is paused by your browser.");
    expect(html).toContain("Tap to enable audio");
    expect(html).toContain("Different conversation");
  });

  test("call audio playback prompt keeps retry affordance after failed browser resume", async () => {
    const component = await loadVueComponent("../src/components/calls/CallAudioPlaybackPrompt.vue", {
      inlineTemplate: false,
    }) as {
      setup: (
        props: Record<string, never>,
        context: { expose: () => void },
      ) => {
        enableAudio: () => Promise<void>;
        visible: { value: boolean };
        resumeFailed: { value: boolean };
        resuming: { value: boolean };
      };
    };
    let shouldFail = true;
    (useCallEngine().engine as unknown as { startAudio: () => Promise<void> }).startAudio = async () => {
      if (shouldFail) throw new Error("blocked");
    };
    const bindings = component.setup({}, { expose: () => undefined });

    await bindings.enableAudio();

    expect(bindings.resumeFailed.value).toBe(true);
    expect(bindings.resuming.value).toBe(false);
    const failedRenderBindings = {
      ...bindings,
      visible: true,
      resumeFailed: bindings.resumeFailed.value,
      resuming: bindings.resuming.value,
    };
    expect(await renderVueComponentWithBindings(component, failedRenderBindings)).toContain(
      "Try again from this button.",
    );
    expect(await renderVueComponentWithBindings(component, failedRenderBindings)).toContain(
      "Tap to enable audio",
    );

    shouldFail = false;
    await bindings.enableAudio();

    expect(bindings.resumeFailed.value).toBe(false);
    const recoveredRenderBindings = {
      ...bindings,
      visible: true,
      resumeFailed: bindings.resumeFailed.value,
      resuming: bindings.resuming.value,
    };
    expect(await renderVueComponentWithBindings(component, recoveredRenderBindings)).not.toContain(
      "Try again from this button.",
    );
  });

  test("call audio resume helper reports failed browser resume without throwing", async () => {
    const failures: string[] = [];
    const target = {
      async startAudio() {
        throw new Error("blocked");
      },
    };

    await resumeCallAudioPlayback(target, () => {
      failures.push("failed");
    });

    expect(failures).toEqual(["failed"]);
  });

  test("call audio resume attempts ignore settlements after the active call changes", () => {
    const guard = new CallAudioResumeAttemptGuard();
    const callA = guard.begin("call-a");

    expect(callA.matches("call-a")).toBe(true);
    expect(callA.matches("call-b")).toBe(false);

    const callB = guard.begin("call-b");

    expect(callA.matches("call-a")).toBe(false);
    expect(callA.matches("call-b")).toBe(false);
    expect(callB.matches("call-b")).toBe(true);
  });

  test("call audio playback prompt ignores delayed resume failure after call changes", async () => {
    let rejectResume: ((error: Error) => void) | null = null;
    const blocked = ref(true);
    const callState = ref({
      phase: "active",
      peer: "bob@waddle.test/web",
      sid: "call-a",
      media: { audio: true, video: false },
      join,
      kind: "dm",
    } as const);
    const controller = useCallAudioPlaybackPromptController(blocked, callState, {
      startAudio: () =>
        new Promise((_resolve, reject) => {
          rejectResume = reject;
        }),
    });

    const pending = controller.enableAudio();

    expect(controller.resuming.value).toBe(true);

    callState.value = {
      phase: "active",
      peer: "carol@waddle.test/web",
      sid: "call-b",
      media: { audio: true, video: false },
      join,
      kind: "dm",
    };

    expect(controller.resuming.value).toBe(false);

    rejectResume?.(new Error("blocked"));
    await pending;

    expect(controller.activeCallSid.value).toBe("call-b");
    expect(controller.resumeFailed.value).toBe(false);
    expect(controller.resuming.value).toBe(false);
  });

  test("ready shell owns one in-viewport audio recovery banner", async () => {
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/web",
      sid: "dm-call",
      media: { audio: true, video: false },
      join,
      kind: "dm",
    });
    $callUiMode.set("split");
    $callAudioPlaybackBlocked.set(true);
    const appShellSource = readFileSync(
      new URL("../src/components/AppShell.vue", import.meta.url),
      "utf8",
    );
    const readyShellSource = readFileSync(
      new URL("../src/components/chat/ChatReadyShell.vue", import.meta.url),
      "utf8",
    );
    const promptHtml = await renderVueComponent(
      await loadVueComponent("../src/components/calls/CallAudioPlaybackPrompt.vue"),
    );
    const surfaceHtml = await renderSfcComponent(
      "../src/components/calls/CallSplitContainer.vue",
      { dmPeerJid: "bob@waddle.test" },
      import.meta.url,
    );
    const shellStart = readyShellSource.indexOf('<div v-else class="chat-app-shell">');
    const promptMount = readyShellSource.indexOf("<CallAudioPlaybackPrompt />");
    const desktopShell = readyShellSource.indexOf('<div class="chat-desktop-shell">');

    expect(appShellSource).not.toContain("<CallAudioPlaybackPrompt />");
    expect(shellStart).toBeGreaterThanOrEqual(0);
    expect(promptMount).toBeGreaterThan(shellStart);
    expect(promptMount).toBeLessThan(desktopShell);
    expect(`${promptHtml}${surfaceHtml}`.match(/Tap to enable audio/g)?.length ?? 0).toBe(1);
  });
});

describe("call control bar participants toggle", () => {
  test("renders an accessible participants button reflecting the closed dock state", async () => {
    const html = await renderCallControls({ participantsOpen: false, participantCount: 3 });

    expect(html).toContain('aria-label="Open participants"');
    expect(html).toContain('aria-pressed="false"');
    expect(html).toContain('aria-expanded="false"');
    // The live attendee count rides on the button.
    expect(html).toContain(">3<");
  });

  test("reflects the open dock state on the participants button", async () => {
    const html = await renderCallControls({ participantsOpen: true, participantCount: 3 });

    expect(html).toContain('aria-label="Close participants"');
    expect(html).toContain('aria-pressed="true"');
    expect(html).toContain('aria-expanded="true"');
  });
});

describe("call control bar chat toggle", () => {
  test("renders an accessible chat button reflecting the closed dock state", async () => {
    const html = await renderCallControls({ chatOpen: false });

    expect(html).toContain('aria-label="Open chat"');
  });

  test("reflects the open Chat tab state on the chat button", async () => {
    const html = await renderCallControls({ chatOpen: true });

    expect(html).toContain('aria-label="Close chat"');
    expect(html).toMatch(/aria-label="Close chat"[^>]*aria-pressed="true"/);
  });

  test("shows an unread badge on the chat button only when there are unread messages", async () => {
    // The unread count is folded into the button's own aria-label (a nested
    // badge label would be ignored once the button has an aria-label), and the
    // visual pill is aria-hidden.
    const withUnread = await renderCallControls({ chatUnread: 5 });
    expect(withUnread).toContain('aria-label="Open chat, 5 unread messages"');
    expect(withUnread).toContain(">5<");

    const oneUnread = await renderCallControls({ chatUnread: 1 });
    expect(oneUnread).toContain('aria-label="Open chat, 1 unread message"');
    expect(oneUnread).not.toContain("1 unread messages");

    const noUnread = await renderCallControls({ chatUnread: 0 });
    expect(noUnread).not.toContain("unread message");
  });

  test("wires the chat toggle emit", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallControls.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain("toggleChat");
    expect(source).toContain("chatOpen");
    expect(source).toContain("chatUnread");
  });
});

describe("call control bar Picture-in-Picture toggle", () => {
  test("hides the PiP button when no browser PiP mode is available", async () => {
    const html = await renderCallControls({ pictureInPictureSupported: false });

    expect(html).not.toContain("Open Picture-in-Picture");
  });

  test("renders inactive and active PiP states accessibly", async () => {
    const inactive = await renderCallControls({
      pictureInPictureSupported: true,
      pictureInPictureActive: false,
    });
    expect(inactive).toContain('aria-label="Open Picture-in-Picture"');
    expect(inactive).toMatch(/aria-label="Open Picture-in-Picture"[^>]*aria-pressed="false"/);

    const active = await renderCallControls({
      pictureInPictureSupported: true,
      pictureInPictureActive: true,
    });
    expect(active).toContain('aria-label="Return to call"');
    expect(active).toMatch(/aria-label="Return to call"[^>]*aria-pressed="true"/);
  });

  test("wires the PiP toggle emit", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallControls.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain("togglePictureInPicture");
    expect(source).toContain("pictureInPictureSupported");
    expect(source).toContain("pictureInPictureActive");
  });
});

describe("call control bar restructure (#1020)", () => {
  test("no longer embeds the connection-quality indicator (now in the stage-header)", () => {
    // The indicator moved to CallStageHeader (asserted there via the recursive
    // SFC harness). This bar must not import or render it any more.
    const source = readFileSync(
      new URL("../src/components/calls/CallControls.vue", import.meta.url),
      "utf8",
    );
    expect(source).not.toContain("CallConnectionIndicator");
  });

  test("exposes a More overflow trigger that opens a menu", async () => {
    const html = await renderCallControls({});
    expect(html).toContain('aria-haspopup="menu"');
    expect(html).toContain('aria-label="More options"');
    // Collapsed at rest.
    expect(html).toMatch(/aria-haspopup="menu"[^>]*aria-expanded="false"/);
  });

  test("holds the Settings action as a menu item under More, not as a top-level button", async () => {
    const html = await renderCallControls({});
    // Settings is a menu item now…
    expect(html).toContain('role="menuitem"');
    expect(html).toContain("Call settings");
  });

  test("wires the More menu's Settings item to the openSettings emit", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallControls.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain("openSettings");
    expect(source).toContain('role="menu"');
  });
});

describe("call control bar hide self-view (#1021)", () => {
  test("offers a checkable Self-view item under More, checked while the self-view is visible", async () => {
    const html = await renderCallControls({ selfViewHidden: false });
    // A menuitemcheckbox with a stable label and aria-checked carrying state,
    // matching the codebase's other in-menu binary toggles — so a screen reader
    // announces the current state, not just the next action.
    expect(html).toContain('role="menuitemcheckbox"');
    expect(html).toContain("Self-view");
    expect(html).toMatch(/role="menuitemcheckbox"[^>]*aria-checked="true"/);
  });

  test("unchecks the Self-view item once the self-view is hidden", async () => {
    const html = await renderCallControls({ selfViewHidden: true });
    expect(html).toContain("Self-view");
    expect(html).toMatch(/role="menuitemcheckbox"[^>]*aria-checked="false"/);
  });

  test("keeps the checkable Self-view item in the roving-focus set with the other menu items", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallControls.vue", import.meta.url),
      "utf8",
    );
    // The menu's keyboard navigation must collect both plain items and the
    // checkbox item, or the toggle drops out of arrow-key focus order.
    expect(source).toContain('[role="menuitemcheckbox"]');
  });

  test("wires the self-view item to the toggleSelfView emit", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallControls.vue", import.meta.url),
      "utf8",
    );
    expect(source).toContain("toggleSelfView");
    expect(source).toContain("selfViewHidden");
  });
});

describe("call control bar immersive mode (#1028)", () => {
  test("expanded mode offers Immersive without browser fullscreen controls", async () => {
    const html = await renderCallControls({ isExpanded: true, isImmersive: false });
    expect(html).toContain("Collapse call to split view");
    expect(html).toContain("Make call immersive");
    expect(html).not.toContain("Enter browser fullscreen");
    expect(html).not.toContain("Exit browser fullscreen");
  });

  test("immersive mode offers a return action and browser fullscreen entry", async () => {
    const html = await renderCallControls({
      isExpanded: true,
      isImmersive: true,
      isNativeFullscreen: false,
    });
    expect(html).toContain("Return call to expanded view");
    expect(html).toContain("Enter browser fullscreen");
    expect(html).not.toContain("Exit browser fullscreen");
  });

  test("immersive native fullscreen action switches to exit while active", async () => {
    const html = await renderCallControls({
      isExpanded: true,
      isImmersive: true,
      isNativeFullscreen: true,
    });
    expect(html).toContain("Exit browser fullscreen");
    expect(html).not.toContain("Enter browser fullscreen");
  });
});

async function renderCallControls(overrides: Record<string, unknown>): Promise<string> {
  const component = await loadVueComponent("../src/components/calls/CallControls.vue");
  const props = {
    micEnabled: true,
    camEnabled: true,
    screenShareEnabled: false,
    screenShareSupported: true,
    isExpanded: false,
    participantsOpen: false,
    participantCount: 0,
    chatOpen: false,
    chatUnread: 0,
    viewMode: "gallery",
    selfViewHidden: false,
    ...overrides,
  };
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

function setActiveMucCall(): void {
  $callState.set({
    phase: "active",
    peer: "general@conference.example.com",
    sid: "c1",
    media: { audio: true, video: false },
    join,
    kind: "muc",
    selfNick: "alice",
  });
}

async function renderVueComponent(component: unknown): Promise<string> {
  return renderToString(createSSRApp({ render: () => h(component as never) }));
}

async function renderVueComponentWithBindings(
  component: { render: unknown },
  bindings: Record<string, unknown>,
): Promise<string> {
  return renderToString(createSSRApp({
    render: () =>
      (component.render as (
        ctx: Record<string, unknown>,
        cache: unknown[],
        props: Record<string, unknown>,
        setup: Record<string, unknown>,
        data: Record<string, unknown>,
        options: Record<string, unknown>,
      ) => unknown)(bindings, [], {}, bindings, {}, {}),
  }));
}

async function loadVueComponent(
  path: string,
  options: { inlineTemplate?: boolean } = { inlineTemplate: true },
) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: options.inlineTemplate ?? true,
  });

  const tempDir = mkdtempSync(joinPath(tmpdir(), "waddle-call-audio-playback-"));
  try {
    const parts = [
      rewriteImports(script.content.replace("export default", "const __sfc__ ="), filename),
    ];
    if (!(options.inlineTemplate ?? true) && descriptor.template) {
      const template = compileTemplate({
        id: filename.pathname,
        filename: filename.pathname,
        source: descriptor.template.content,
      }).code.replace("export function render", "function render");
      parts.push(rewriteImports(template, filename), "__sfc__.render = render;");
    }
    parts.push("export default __sfc__;");
    const compiled = parts.join("\n");
    const js = ts.transpileModule(compiled, {
      compilerOptions: {
        module: ts.ModuleKind.ESNext,
        target: ts.ScriptTarget.ES2022,
        verbatimModuleSyntax: false,
      },
    }).outputText;
    const modulePath = joinPath(tempDir, "Component.mjs");
    writeFileSync(modulePath, js);
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function rewriteImports(code: string, importer: URL): string {
  // This SFC test helper only supports the static import shapes emitted by
  // vue/compiler-sfc for these components today. If compiler output starts
  // using dynamic/template-literal imports, prefer a shared component-test
  // harness over broadening this regex.
  return code.replace(/from\s+["']([^"']+)["']/g, (_match, specifier: string) =>
    `from ${JSON.stringify(resolveModuleSpecifier(specifier, importer))}`);
}

function resolveModuleSpecifier(specifier: string, importer: URL): string {
  if (specifier.startsWith("@/")) {
    return moduleUrlForPath(resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname));
  }
  if (specifier.startsWith(".")) {
    return moduleUrlForPath(resolveSourcePath(new URL(specifier, importer).pathname));
  }
  return moduleUrlForPath(require.resolve(specifier));
}

function resolveSourcePath(path: string): string {
  if (existsSync(path)) return path;
  if (existsSync(`${path}.ts`)) return `${path}.ts`;
  if (existsSync(`${path}.vue`)) return `${path}.vue`;
  return path;
}

function moduleUrlForPath(path: string): string {
  return pathToFileURL(path).href;
}
