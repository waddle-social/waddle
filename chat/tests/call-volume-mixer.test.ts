import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { parse, compileScript } from "vue/compiler-sfc";
import ts from "typescript";
import {
  buildCallVolumeMixerRows,
  callVolumePercentToGain,
  resetCallVolumeMixerLevels,
  type CallVolumeLevelStore,
} from "../src/lib/calls/call-volume-mixer";

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
    expect(callVolumePercentToGain(99, 0.98)).toBe(1);
    expect(callVolumePercentToGain(99, 1)).toBe(0.99);
    expect(callVolumePercentToGain(100, 0.99)).toBe(1);
    expect(callVolumePercentToGain(101, 1)).toBe(1.01);
    expect(callVolumePercentToGain(101, 1.02)).toBe(1);
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
      const emitted: Array<[string, CallVolumeMixerRow, number]> = [];
      const bindings = setup({ rows: [] }, {
        expose: () => undefined,
        emit: (event: string, row: CallVolumeMixerRow, level: number) => {
          emitted.push([event, row, level]);
        },
      });

      const rowBelow = mixerRow({ level: 0.98 });
      const rowNeutral = mixerRow({ level: 1 });
      const rowAbove = mixerRow({ level: 1.02 });
      bindings.onInput(rowBelow, inputEvent("99"));
      bindings.onInput(rowNeutral, inputEvent("99"));
      bindings.onInput(rowNeutral, inputEvent("101"));
      bindings.onInput(rowAbove, inputEvent("101"));

      expect(emitted.map(([, , level]) => level)).toEqual([1, 0.99, 1.01, 1]);
    } finally {
      globalThis.HTMLInputElement = originalHTMLInputElement;
    }
  });
});

describe("expanded call mixer wiring", () => {
  test("clears remembered levels on call changes and resets absent engine targets", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallExpandedSurface.vue", import.meta.url),
      "utf8",
    );

    expect(source).toContain("const activeCallSid = computed");
    expect(source).toContain("watch(activeCallSid");
    expect(source).toContain("participantAudioTargets.value = {}");
    expect(source).toContain("for (const target of Object.values(participantAudioTargets.value))");
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
    emit: (event: string, row: CallVolumeMixerRow, level: number) => void;
  },
) => { onInput: (row: CallVolumeMixerRow, event: Event) => void }> {
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
