import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { parse, compileScript } from "vue/compiler-sfc";
import ts from "typescript";
import { renderVueComponent } from "./helpers/render-vue-sfc";
import { buildCallRoster, type CallRosterRow } from "../src/lib/calls/call-roster";
import { buildCallVolumeMixerRows, type CallVolumeMixerRow } from "../src/lib/calls/call-volume-mixer";
import type { CallTrackSource, RemoteMediaTrack } from "../src/lib/calls/engine";

const require = createRequire(import.meta.url);
const fakeTrack = {} as never;

function remoteTrack(
  publicationSid: string,
  participantIdentity: string,
  kind: "audio" | "video",
  source: CallTrackSource,
): RemoteMediaTrack {
  return { participantIdentity, publicationSid, kind, source, track: fakeTrack };
}

function rosterFixture() {
  const identities = ["alice@waddle.test/web", "bob@waddle.test/desktop"];
  const tracks = [
    remoteTrack("alice-mic", "alice@waddle.test/web", "audio", "microphone"),
    remoteTrack("alice-cam", "alice@waddle.test/web", "video", "camera"),
    remoteTrack("alice-screen", "alice@waddle.test/web", "audio", "screen_share_audio"),
  ];
  const volumeRows = buildCallVolumeMixerRows({
    remoteParticipantIdentities: identities,
    remoteTracks: tracks,
    localIdentity: "me@waddle.test/browser",
    levels: {
      "alice@waddle.test/web:microphone": 1.5,
      // A zero level on the screen-share audio surfaces the muted state.
      "alice@waddle.test/web:screen_share_audio": 0,
    },
  });
  return buildCallRoster({
    remoteParticipantIdentities: identities,
    remoteTracks: tracks,
    localIdentity: "me@waddle.test/browser",
    localMicEnabled: true,
    localCameraEnabled: false,
    activeSpeakerIdentities: new Set<string>(["alice@waddle.test/web"]),
    volumeRows,
  });
}

describe("CallParticipantsPanel", () => {
  test("lists every attendee with live mic/camera state, speaking, and per-participant volume", async () => {
    const html = await renderVueComponent(
      "../src/components/calls/CallParticipantsPanel.vue",
      { rows: rosterFixture() },
      import.meta.url,
    );

    // Every attendee is listed, self first.
    expect(html).toContain("You");
    expect(html).toContain("alice");
    expect(html).toContain("bob");

    // Live mic + camera state is exposed accessibly.
    expect(html).toContain('aria-label="Microphone on"');
    expect(html).toContain('aria-label="Microphone off"');
    expect(html).toContain('aria-label="Camera on"');
    expect(html).toContain('aria-label="Camera off"');

    // The active speaker is flagged.
    expect(html).toContain('aria-label="Speaking"');

    // A working per-participant volume slider, reflecting the stored level.
    expect(html).toContain('aria-label="Volume for alice"');
    expect(html).toContain('aria-valuetext="150%"');
    expect(html).toContain('max="200"');

    // Screen-share audio is a distinct per-participant control, muted at 0%.
    expect(html).toContain("alice&#39;s screen");
    expect(html).toContain('aria-label="Volume for alice&#39;s screen"');
    expect(html).toContain('aria-label="Muted"');

    // bob's mic is off, so his volume row is disabled with a hint.
    expect(html).toContain("mic off");
    expect(html).toContain("disabled");

    expect(html).toContain("Reset all");
  });

  test("the slider snaps directional gains via the 100% detent and emits setVolume/resetAll", async () => {
    const originalHTMLInputElement = globalThis.HTMLInputElement;
    globalThis.HTMLInputElement = TestInputElement as typeof HTMLInputElement;
    try {
      const setup = await loadParticipantsPanelSetup();
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
      // Each input echoes the last emitted gain, so the detent snaps
      // relative to where the user is dragging from — not the prop.
      bindings.onInput(rowBelow, inputEvent("99")); // 0.98 → 1 (snap up)
      bindings.onInput(rowNeutral, inputEvent("99")); // 1 → 0.99 (no snap down through detent)
      bindings.onInput(rowAbove, inputEvent("101")); // 1.02 → 1 (snap down)
      bindings.onResetAll();
      // After reset the echo is cleared, so the next input snaps from the prop level.
      bindings.onInput(staleRowBelow, inputEvent("99")); // 0.5 prop → 0.99

      expect(emitted.map(([event, , level]) => [event, level])).toEqual([
        ["setVolume", 1],
        ["setVolume", 0.99],
        ["setVolume", 1],
        ["resetAll", undefined],
        ["setVolume", 0.99],
      ]);
    } finally {
      globalThis.HTMLInputElement = originalHTMLInputElement;
    }
  });
});

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
  return { target: new TestInputElement(value) } as unknown as Event;
}

class TestInputElement {
  constructor(readonly value: string) {}
}

type PanelBindings = {
  onInput: (row: CallVolumeMixerRow, event: Event) => void;
  onResetAll: () => void;
  syncLastEmittedLevels: (rows: readonly CallRosterRow[]) => void;
};

async function loadParticipantsPanelSetup(): Promise<(
  props: { rows: readonly CallRosterRow[] },
  context: {
    expose: () => void;
    emit: (event: string, row?: CallVolumeMixerRow, level?: number) => void;
  },
) => PanelBindings> {
  const component = await loadVueComponentScript(
    "../src/components/calls/CallParticipantsPanel.vue",
  );
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

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-participants-panel-"));
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

function rewriteImports(code: string, importer: URL): string {
  return code.replace(/from\s+["']([^"']+)["']/g, (_match, specifier: string) =>
    `from ${JSON.stringify(resolveModuleSpecifier(specifier, importer))}`);
}

function resolveModuleSpecifier(specifier: string, importer: URL): string {
  if (specifier.startsWith("@/")) {
    return pathToFileURL(
      resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname),
    ).href;
  }
  if (specifier.startsWith(".")) {
    return pathToFileURL(new URL(specifier, importer).pathname).href;
  }
  return pathToFileURL(require.resolve(specifier)).href;
}

function resolveSourcePath(path: string): string {
  if (existsSync(path)) return path;
  if (existsSync(`${path}.ts`)) return `${path}.ts`;
  if (existsSync(`${path}.vue`)) return `${path}.vue`;
  return path;
}
