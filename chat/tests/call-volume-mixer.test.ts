import { afterEach, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { computed, createSSRApp, effectScope, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { parse, compileScript } from "vue/compiler-sfc";
import ts from "typescript";
import {
  buildCallVolumeMixerRows,
  callVolumePercentToGain,
  resetCallVolumeMixerLevels,
  type CallVolumeLevelStore,
} from "../src/lib/calls/call-volume-mixer";
import { useCallVolumeMixer } from "../src/lib/calls/use-call-volume-mixer";
import { useCallEngine } from "../src/lib/calls/use-call-engine";
import { $callState, clearCallState } from "../src/lib/calls/call-store";

const fakeTrack = {} as never;
const require = createRequire(import.meta.url);

describe("call volume mixer projection", () => {
  test("groups each remote participant's voice before screen-share audio and excludes self", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: [
        "carol@waddle.test/tablet",
        "alice@waddle.test/web",
        "bob@waddle.test/desktop",
      ],
      remoteTracks: [
        remoteAudio("bob-mic", "bob@waddle.test/desktop", "microphone"),
        remoteAudio("bob-screen", "bob@waddle.test/desktop", "screen_share_audio"),
        remoteAudio("alice-screen", "alice@waddle.test/web", "screen_share_audio"),
        remoteAudio("self-mic", "me@waddle.test/browser", "microphone"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/desktop:microphone": 0.4,
        "bob@waddle.test/desktop:screen_share_audio": 0.7,
        "alice@waddle.test/web:screen_share_audio": 0,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      label: row.label,
      source: row.source,
      level: row.level,
      disabled: row.disabled,
      hint: row.hint,
      muted: row.muted,
    }))).toEqual([
      {
        key: "carol@waddle.test/tablet:microphone",
        label: "carol",
        source: "microphone",
        level: 1,
        disabled: true,
        hint: "mic off",
        muted: false,
      },
      {
        key: "alice@waddle.test/web:microphone",
        label: "alice",
        source: "microphone",
        level: 1,
        disabled: true,
        hint: "mic off",
        muted: false,
      },
      {
        key: "alice@waddle.test/web:screen_share_audio",
        label: "alice's screen",
        source: "screen_share_audio",
        level: 0,
        disabled: false,
        hint: null,
        muted: true,
      },
      {
        key: "bob@waddle.test/desktop:microphone",
        label: "bob",
        source: "microphone",
        level: 0.4,
        disabled: false,
        hint: null,
        muted: false,
      },
      {
        key: "bob@waddle.test/desktop:screen_share_audio",
        label: "bob's screen",
        source: "screen_share_audio",
        level: 0.7,
        disabled: false,
        hint: null,
        muted: false,
      },
    ]);
  });

  test("dedupes normalized participant snapshots against differently-cased LiveKit track identities", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["bob@waddle.test/Desktop"],
      remoteTracks: [
        remoteAudio("bob-mic", "Bob@Waddle.Test/Desktop", "microphone"),
        remoteAudio("bob-screen", "Bob@Waddle.Test/Desktop", "screen_share_audio"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/Desktop:microphone": 0.25,
        "bob@waddle.test/Desktop:screen_share_audio": 0.5,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      participantIdentity: row.participantIdentity,
      label: row.label,
      source: row.source,
      level: row.level,
      disabled: row.disabled,
    }))).toEqual([
      {
        key: "bob@waddle.test/Desktop:microphone",
        participantIdentity: "Bob@Waddle.Test/Desktop",
        label: "Bob",
        source: "microphone",
        level: 0.25,
        disabled: false,
      },
      {
        key: "bob@waddle.test/Desktop:screen_share_audio",
        participantIdentity: "Bob@Waddle.Test/Desktop",
        label: "Bob's screen",
        source: "screen_share_audio",
        level: 0.5,
        disabled: false,
      },
    ]);
  });

  test("keeps remembered mic level visible while a differently-cased LiveKit identity is muted", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["bob@waddle.test/Desktop"],
      remoteTracks: [],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "bob@waddle.test/Desktop:microphone": 0.25,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      level: row.level,
      disabled: row.disabled,
      hint: row.hint,
      ariaValueText: row.ariaValueText,
    }))).toEqual([
      {
        key: "bob@waddle.test/Desktop:microphone",
        level: 0.25,
        disabled: true,
        hint: "mic off",
        ariaValueText: "25%",
      },
    ]);
  });

  test("normalizes remembered levels to the 0-200 percent gain range", () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["alice@waddle.test/web", "bob@waddle.test/desktop"],
      remoteTracks: [
        remoteAudio("alice-mic", "alice@waddle.test/web", "microphone"),
        remoteAudio("bob-mic", "bob@waddle.test/desktop", "microphone"),
        remoteAudio("bob-screen", "bob@waddle.test/desktop", "screen_share_audio"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "alice@waddle.test/web:microphone": 3,
        "bob@waddle.test/desktop:microphone": Number.NaN,
        "bob@waddle.test/desktop:screen_share_audio": -0.5,
      },
    });

    expect(rows.map((row) => ({
      key: row.key,
      level: row.level,
      ariaValueText: row.ariaValueText,
    }))).toEqual([
      {
        key: "alice@waddle.test/web:microphone",
        level: 2,
        ariaValueText: "200%",
      },
      {
        key: "bob@waddle.test/desktop:microphone",
        level: 1,
        ariaValueText: "100%",
      },
      {
        key: "bob@waddle.test/desktop:screen_share_audio",
        level: 0,
        ariaValueText: "0%",
      },
    ]);
  });
});

describe("call volume mixer reducer", () => {
  test("maps slider percentages to gain with a 100 percent snap detent", () => {
    expect(callVolumePercentToGain(0)).toBe(0);
    expect(callVolumePercentToGain(88)).toBe(0.88);
    expect(callVolumePercentToGain(99, 0.5)).toBe(0.99);
    expect(callVolumePercentToGain(99, 0.98)).toBe(1);
    expect(callVolumePercentToGain(99, 1)).toBe(0.99);
    expect(callVolumePercentToGain(100, 0.99)).toBe(1);
    expect(callVolumePercentToGain(101, 1)).toBe(1.01);
    expect(callVolumePercentToGain(101, 1.02)).toBe(1);
    expect(callVolumePercentToGain(101, 1.5)).toBe(1.01);
    expect(callVolumePercentToGain(150)).toBe(1.5);
    expect(callVolumePercentToGain(250)).toBe(2);
    expect(callVolumePercentToGain(Number.NaN)).toBe(1);
  });

  test("reset all returns every stored participant audio entry to full volume", () => {
    const levels: CallVolumeLevelStore = {
      "alice@waddle.test/web:microphone": 0.2,
      "alice@waddle.test/web:screen_share_audio": 0,
      "bob@waddle.test/desktop:microphone": 0.8,
    };

    expect(resetCallVolumeMixerLevels(levels)).toEqual({
      "alice@waddle.test/web:microphone": 1,
      "alice@waddle.test/web:screen_share_audio": 1,
      "bob@waddle.test/desktop:microphone": 1,
    });
  });
});

describe("call volume mixer panel", () => {
  test("renders accessible row sliders, muted state, 100% ticks, and reset-all footer", async () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["alice@waddle.test/web", "bob@waddle.test/desktop"],
      remoteTracks: [
        remoteAudio("alice-mic", "alice@waddle.test/web", "microphone"),
        remoteAudio("alice-screen", "alice@waddle.test/web", "screen_share_audio"),
        remoteAudio("bob-screen", "bob@waddle.test/desktop", "screen_share_audio"),
      ],
      localIdentity: "me@waddle.test/browser",
      levels: {
        "alice@waddle.test/web:screen_share_audio": 0,
        "alice@waddle.test/web:microphone": 1.5,
        "bob@waddle.test/desktop:microphone": 0.35,
      },
    });

    const html = await renderCallVolumeMixerPanel({ rows });

    expect(html).toContain("Who you hear");
    expect(html).toContain("alice");
    expect(html).toContain("alice&#39;s screen");
    expect(html).toContain("bob&#39;s screen");
    expect(html).toContain("mic off");
    expect(html).toContain('aria-label="Volume for alice"');
    expect(html).toContain('max="200"');
    expect(html).toContain('aria-valuetext="150%"');
    expect(html).toContain('aria-label="Volume for alice&#39;s screen"');
    expect(html).toContain('aria-valuetext="0%"');
    expect(html).toContain('aria-label="Muted"');
    expect(html).toContain('class="call-volume-mixer__tick"');
    expect(html).toContain('style="left:50%;"');
    expect(html).toContain("Reset all");
  });

  test("emits directional snap-detent gains from slider input", async () => {
    const originalHTMLInputElement = globalThis.HTMLInputElement;
    globalThis.HTMLInputElement = TestInputElement as typeof HTMLInputElement;
    try {
      const setup = await loadCallVolumeMixerPanelSetup();
      const emitted: Array<[string, CallVolumeMixerRow | undefined, number | undefined]> = [];
      const bindings = setup({ rows: [] }, {
        expose: () => undefined,
        emit: (event: string, row?: CallVolumeMixerRow, level?: number) => {
          emitted.push([event, row, level]);
        },
      });

      const rowBelow = mixerRow({ key: "below", level: 0.98 });
      const rowNeutral = mixerRow({ key: "neutral", level: 1 });
      const rowAbove = mixerRow({ key: "above", level: 1.02 });
      const staleRowBelow = mixerRow({ key: "stale-low", level: 0.5 });
      const staleRowAbove = mixerRow({ key: "stale-high", level: 1.5 });
      bindings.onInput(rowBelow, inputEvent("99"));
      bindings.onInput(rowNeutral, inputEvent("99"));
      bindings.onInput(rowNeutral, inputEvent("101"));
      bindings.onInput(rowAbove, inputEvent("101"));
      bindings.onInput(staleRowBelow, inputEvent("102"));
      bindings.onInput(staleRowBelow, inputEvent("101"));
      bindings.onInput(staleRowAbove, inputEvent("98"));
      bindings.onInput(staleRowAbove, inputEvent("99"));
      bindings.onResetAll();
      bindings.onInput(staleRowBelow, inputEvent("99"));
      bindings.onInput(staleRowBelow, inputEvent("102"));
      bindings.syncLastEmittedLevels([mixerRow({ key: "stale-low", level: 1 })]);
      bindings.onInput(staleRowBelow, inputEvent("99"));

      expect(emitted.map(([event, , level]) => [event, level])).toEqual([
        ["setVolume", 1],
        ["setVolume", 0.99],
        ["setVolume", 1.01],
        ["setVolume", 1],
        ["setVolume", 1.02],
        ["setVolume", 1],
        ["setVolume", 0.98],
        ["setVolume", 1],
        ["resetAll", undefined],
        ["setVolume", 0.99],
        ["setVolume", 1.02],
        ["setVolume", 0.99],
      ]);
      expect(emitted.map(([, , level]) => level)).toEqual([
        1,
        0.99,
        1.01,
        1,
        1.02,
        1,
        0.98,
        1,
        undefined,
        0.99,
        1.02,
        0.99,
      ]);
    } finally {
      globalThis.HTMLInputElement = originalHTMLInputElement;
    }
  });
});

describe("call volume mixer controller apply path", () => {
  const join = {
    url: "wss://livekit.test",
    room: "bob@waddle.test::c1",
    identity: "me@waddle.test/browser",
    token: "jwt",
  };

  afterEach(() => {
    // Idle resets the module-scoped remembered levels via the
    // controller's own SID-change subscription, isolating each test.
    clearCallState();
  });

  test("setVolume applies the chosen gain to the engine and resetAll returns it to unity", () => {
    const captured = captureEngineVolumeCalls();
    const scope = effectScope();
    try {
      activateDmCall("c1");
      let controller!: ReturnType<typeof useCallVolumeMixer>;
      scope.run(() => {
        controller = useCallVolumeMixer(computed(() => ""));
      });

      controller.setVolume(
        mixerRow({
          key: "bob@waddle.test/desktop:microphone",
          participantIdentity: "bob@waddle.test/desktop",
          source: "microphone",
        }),
        0.5,
      );
      expect(captured.calls).toContainEqual({
        participantIdentity: "bob@waddle.test/desktop",
        source: "microphone",
        volume: 0.5,
      });

      captured.calls.length = 0;
      controller.resetAll();
      expect(captured.calls).toContainEqual({
        participantIdentity: "bob@waddle.test/desktop",
        source: "microphone",
        volume: 1,
      });
    } finally {
      scope.stop();
      captured.restore();
    }
  });

  test("remembered gain is shared across surfaces and wiped when the call SID changes", () => {
    const captured = captureEngineVolumeCalls();
    const splitScope = effectScope();
    const expandedScope = effectScope();
    const bob = mixerRow({
      key: "bob@waddle.test/desktop:microphone",
      participantIdentity: "bob@waddle.test/desktop",
      source: "microphone",
    });
    try {
      // A MUC call with no live participants projects no rows, so
      // reset-all touches the engine only via the remembered targets —
      // isolating the shared cross-surface state from row projection.
      activateMucCall("c1");
      let split!: ReturnType<typeof useCallVolumeMixer>;
      let expanded!: ReturnType<typeof useCallVolumeMixer>;
      splitScope.run(() => {
        split = useCallVolumeMixer(computed(() => "room@muc.waddle.test"));
      });
      expandedScope.run(() => {
        expanded = useCallVolumeMixer(computed(() => "room@muc.waddle.test"));
      });

      // A gain set from the split surface is remembered in shared state,
      // so reset-all from the EXPANDED surface returns that same target
      // to unity — one source of truth across both surfaces.
      split.setVolume(bob, 0.5);
      captured.calls.length = 0;
      expanded.resetAll();
      expect(captured.calls).toContainEqual({
        participantIdentity: "bob@waddle.test/desktop",
        source: "microphone",
        volume: 1,
      });

      // Switching to a different call forgets the remembered target:
      // reset-all from either surface now touches nothing.
      split.setVolume(bob, 0.5);
      activateMucCall("c2");
      captured.calls.length = 0;
      expanded.resetAll();
      expect(captured.calls).toEqual([]);
    } finally {
      splitScope.stop();
      expandedScope.stop();
      captured.restore();
    }
  });

  function activateDmCall(sid: string): void {
    $callState.set({
      phase: "active",
      peer: "bob@waddle.test/desktop",
      sid,
      media: { audio: true, video: false },
      join,
      kind: "dm",
    });
  }

  function activateMucCall(sid: string): void {
    $callState.set({
      phase: "active",
      peer: "room@muc.waddle.test",
      sid,
      media: { audio: true, video: false },
      join: { ...join, room: "room@muc.waddle.test" },
      kind: "muc",
      selfNick: "me",
      selfFullJid: "me@waddle.test/browser",
    });
  }
});

describe("call volume mixer controller wiring", () => {
  test("shares one mixer controller across both call surfaces", () => {
    const split = readFileSync(
      new URL("../src/components/calls/CallSplitContainer.vue", import.meta.url),
      "utf8",
    );
    const expanded = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    for (const source of [split, expanded]) {
      expect(source).toContain("useCallVolumeMixer");
    }
  });

  test("both surfaces toggle the mixer dialog from the control bar speaker button", () => {
    const split = readFileSync(
      new URL("../src/components/calls/CallSplitContainer.vue", import.meta.url),
      "utf8",
    );
    const expanded = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    for (const source of [split, expanded]) {
      expect(source).toContain("CallVolumeMixerDialog");
      expect(source).toContain(":volume-open=\"volumeOpen\"");
      expect(source).toContain("@toggle-volume=\"volumeOpen = !volumeOpen\"");
      expect(source).toContain("v-model:open=\"volumeOpen\"");
    }
  });

  test("expanded surface drops the always-on sidebar in favour of the toggle dialog", () => {
    const expanded = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    expect(expanded).not.toContain("CallVolumeMixerPanel");
  });

  test("expanded Escape stays on the dialog instead of collapsing the call", () => {
    const expanded = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    const start = expanded.indexOf("function onKeydown");
    const guard = expanded.slice(start, expanded.indexOf("collapseToSplit()", start));
    expect(guard).toContain("settingsOpen.value");
    expect(guard).toContain("volumeOpen.value");
  });
});

function remoteAudio(
  publicationSid: string,
  participantIdentity: string,
  source: "microphone" | "screen_share_audio",
) {
  return {
    participantIdentity,
    publicationSid,
    kind: "audio" as const,
    source,
    track: fakeTrack,
  };
}

async function renderCallVolumeMixerPanel(props: Record<string, unknown>): Promise<string> {
  const component = await loadVueComponent("../src/components/calls/CallVolumeMixerPanel.vue");
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function loadVueComponent(path: string) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: true,
  });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-volume-mixer-"));
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
    const modulePath = join(tempDir, "Component.mjs");
    writeFileSync(modulePath, js);
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

async function loadCallVolumeMixerPanelSetup(): Promise<(
  props: { rows: readonly CallVolumeMixerRow[] },
  context: {
    expose: () => void;
    emit: (event: string, row?: CallVolumeMixerRow, level?: number) => void;
  },
) => {
  onInput: (row: CallVolumeMixerRow, event: Event) => void;
  onResetAll: () => void;
  syncLastEmittedLevels: (rows: readonly CallVolumeMixerRow[]) => void;
}> {
  const component = await loadVueComponentScript("../src/components/calls/CallVolumeMixerPanel.vue");
  return component.setup;
}

async function loadVueComponentScript(path: string) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: false,
  });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-volume-mixer-script-"));
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
    const modulePath = join(tempDir, "Component.mjs");
    writeFileSync(modulePath, js);
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function captureEngineVolumeCalls() {
  const { engine } = useCallEngine();
  const calls: Array<{ participantIdentity: string; source: string; volume: number }> = [];
  const target = engine as unknown as {
    setParticipantAudioVolume: (input: {
      participantIdentity: string;
      source: string;
      volume: number;
    }) => void;
  };
  const original = target.setParticipantAudioVolume;
  target.setParticipantAudioVolume = (input) => {
    calls.push(input);
  };
  return {
    calls,
    restore() {
      target.setParticipantAudioVolume = original;
    },
  };
}

function mixerRow(overrides: Partial<CallVolumeMixerRow>): CallVolumeMixerRow {
  return {
    key: "alice@waddle.test/web:microphone",
    participantIdentity: "alice@waddle.test/web",
    source: "microphone",
    label: "alice",
    level: 1,
    disabled: false,
    hint: null,
    muted: false,
    ariaLabel: "Volume for alice",
    ariaValueText: "100%",
    ...overrides,
  };
}

function inputEvent(value: string): Event {
  return {
    target: new TestInputElement(value),
  } as unknown as Event;
}

class TestInputElement {
  constructor(readonly value: string) {}
}

function rewriteImports(code: string, importer: URL): string {
  return code.replace(/from\s+["']([^"']+)["']/g, (_match, specifier: string) =>
    `from ${JSON.stringify(resolveModuleSpecifier(specifier, importer))}`);
}

function resolveModuleSpecifier(specifier: string, importer: URL): string {
  if (specifier.startsWith("@/")) {
    return moduleUrlForPath(resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname));
  }
  if (specifier.startsWith(".")) {
    return moduleUrlForPath(new URL(specifier, importer).pathname);
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
