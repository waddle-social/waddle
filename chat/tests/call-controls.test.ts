import { afterEach, describe, expect, mock, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join as joinPath } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { parse, compileScript } from "vue/compiler-sfc";
import ts from "typescript";
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
import {
  $callAudioPlaybackBlocked,
  resumeCallAudioPlayback,
} from "../src/lib/calls/call-audio-playback";
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

const require = createRequire(import.meta.url);

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
  $callAudioPlaybackBlocked.set(false);
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
    expect(await renderVueComponent(component)).not.toContain("Tap to enable audio");

    $callAudioPlaybackBlocked.set(true);
    const html = await renderVueComponent(component);
    expect(html).toContain("Audio is paused by your browser.");
    expect(html).toContain("Tap to enable audio");
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

  test("call surfaces mount the tap-to-enable audio affordance", () => {
    const promptSource = readFileSync(
      new URL("../src/components/calls/CallAudioPlaybackPrompt.vue", import.meta.url),
      "utf8",
    );
    const splitSource = readFileSync(
      new URL("../src/components/calls/CallSplitContainer.vue", import.meta.url),
      "utf8",
    );
    const expandedSource = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    expect(promptSource).toContain(":disabled=\"resuming\"");
    expect(splitSource).toContain("<CallAudioPlaybackPrompt />");
    expect(expandedSource).toContain("<CallAudioPlaybackPrompt />");
  });
});

async function renderVueComponent(component: unknown): Promise<string> {
  return renderToString(createSSRApp({ render: () => h(component as never) }));
}

async function loadVueComponent(path: string) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: true,
  });

  const tempDir = mkdtempSync(joinPath(tmpdir(), "waddle-call-audio-playback-"));
  try {
    const compiled = [
      rewriteImports(script.content.replace("export default", "const __sfc__ ="), filename),
      "export default __sfc__;",
    ].join("\n");
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
