import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { parse, compileScript } from "vue/compiler-sfc";
import ts from "typescript";
import { buildCallTiles } from "../src/lib/calls/call-tiles";

const fakeTrack = {} as never;

describe("call tile projection", () => {
  test("projects a remote camera and screen share as distinct tiles", () => {
    const screenAudioTrack = {} as never;
    const tiles = buildCallTiles({
      remoteTracks: [
        {
          participantIdentity: "alice@example.com/web",
          publicationSid: "camera",
          kind: "video",
          source: "camera",
          track: fakeTrack,
        },
        {
          participantIdentity: "alice@example.com/web",
          publicationSid: "screen",
          kind: "video",
          source: "screen_share",
          track: fakeTrack,
        },
        {
          participantIdentity: "alice@example.com/web",
          publicationSid: "screen-audio",
          kind: "audio",
          source: "screen_share_audio",
          track: screenAudioTrack,
        },
      ],
      localTracks: [],
      localIdentity: null,
      micEnabled: true,
    });

    expect(tiles.map((tile) => ({
      key: tile.key,
      label: tile.label,
      source: tile.source,
      isSelf: tile.isSelf,
      mirrorVideo: tile.mirrorVideo,
      showsPresentingGlyph: tile.showsPresentingGlyph,
    }))).toEqual([
      {
        key: "self:you:camera",
        label: "You",
        source: "camera",
        isSelf: true,
        mirrorVideo: true,
        showsPresentingGlyph: false,
      },
      {
        key: "remote:alice@example.com/web:camera",
        label: "alice",
        source: "camera",
        isSelf: false,
        mirrorVideo: false,
        showsPresentingGlyph: false,
      },
      {
        key: "remote:alice@example.com/web:screen_share",
        label: "alice's screen",
        source: "screen_share",
        isSelf: false,
        mirrorVideo: false,
        showsPresentingGlyph: true,
      },
    ]);
    expect(tiles.find((tile) => tile.key === "remote:alice@example.com/web:screen_share")?.audioTrack)
      .toBe(screenAudioTrack);
  });

  test("projects local camera and screen share with separate mirror decisions", () => {
    const tiles = buildCallTiles({
      remoteTracks: [],
      localTracks: [
        {
          participantIdentity: "me@example.com/web",
          publicationSid: "camera",
          kind: "video",
          source: "camera",
          track: fakeTrack,
        },
        {
          participantIdentity: "me@example.com/web",
          publicationSid: "screen",
          kind: "video",
          source: "screen_share",
          track: fakeTrack,
        },
      ],
      localIdentity: "me@example.com/web",
      micEnabled: true,
    });

    expect(tiles.map((tile) => ({
      key: tile.key,
      label: tile.label,
      source: tile.source,
      isSelf: tile.isSelf,
      mirrorVideo: tile.mirrorVideo,
      showsPresentingGlyph: tile.showsPresentingGlyph,
    }))).toEqual([
      {
        key: "self:me@example.com/web:camera",
        label: "You",
        source: "camera",
        isSelf: true,
        mirrorVideo: true,
        showsPresentingGlyph: false,
      },
      {
        key: "self:me@example.com/web:screen_share",
        label: "Your screen",
        source: "screen_share",
        isSelf: true,
        mirrorVideo: false,
        showsPresentingGlyph: true,
      },
    ]);
  });

  test("renders the presenting glyph for screen tiles", async () => {
    const html = await renderCallTile({
      label: "alice's screen",
      attachKey: "remote:alice@example.com/web:screen_share",
      isSelf: false,
      mirrorVideo: false,
      showsPresentingGlyph: true,
      micEnabled: true,
      videoTrack: null,
      audioTrack: null,
      attach: () => undefined,
    });

    expect(html).toContain("alice&#39;s screen");
    expect(html).toContain("lucide-screen-share");
    expect(html).toContain("Open alice&#39;s screen tile");
  });

  test("forwards the presenting glyph flag in every grid branch", () => {
    const source = readFileSync(new URL("../src/components/calls/CallTileGrid.vue", import.meta.url), "utf8");
    expect(source.match(/:shows-presenting-glyph=/g)?.length).toBe(3);
  });
});

async function renderCallTile(props: Record<string, unknown>): Promise<string> {
  const component = await loadVueComponent("../src/components/calls/CallTile.vue");
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

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-tile-grid-"));
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
    return moduleUrlForPath(resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname));
  }
  if (specifier.startsWith(".")) {
    return moduleUrlForPath(resolveSourcePath(new URL(specifier, importer).pathname));
  }
  return import.meta.resolve(specifier);
}

function resolveSourcePath(basePath: string): string {
  const candidates = [
    basePath,
    `${basePath}.ts`,
    `${basePath}.tsx`,
    `${basePath}.js`,
    `${basePath}.mjs`,
    `${basePath}.vue`,
    `${basePath}.json`,
    `${basePath}/index.ts`,
  ];
  const resolved = candidates.find((candidate) => existsSync(candidate));
  if (!resolved) throw new Error(`Unable to resolve test SFC import: ${basePath}`);
  return resolved;
}

function moduleUrlForPath(resolvedPath: string): string {
  return pathToFileURL(resolvedPath).href;
}
