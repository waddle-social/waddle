import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { createSSRApp, h } from "vue";
import { renderToString } from "vue/server-renderer";
import { parse, compileScript } from "vue/compiler-sfc";
import ts from "typescript";
import { buildCallVolumeMixerRows } from "../src/lib/calls/call-volume-mixer";

const require = createRequire(import.meta.url);
const fakeTrack = {} as never;

describe("CallVolumeMixerDialog", () => {
  test("renders the volume mixer panel and an accessible close control when open", async () => {
    const rows = buildCallVolumeMixerRows({
      remoteParticipantIdentities: ["alice@waddle.test/web"],
      remoteTracks: [remoteAudio("alice-mic", "alice@waddle.test/web", "microphone")],
      localIdentity: "me@waddle.test/browser",
      levels: { "alice@waddle.test/web:microphone": 0.5 },
    });

    const html = await renderVueComponent("../src/components/calls/CallVolumeMixerDialog.vue", {
      open: true,
      rows,
      "onUpdate:open": () => undefined,
    });

    expect(html).toContain("Who you hear");
    expect(html).toContain("alice");
    expect(html).toContain('aria-label="Volume for alice"');
    expect(html).toContain('aria-label="Close volume mixer"');
    expect(html).toContain("Reset all");
  });

  test("forwards the panel's set-volume and reset-all events to the parent", () => {
    const source = readFileSync(
      new URL("../src/components/calls/CallVolumeMixerDialog.vue", import.meta.url),
      "utf8",
    );

    expect(source).toContain("@set-volume");
    expect(source).toContain("@reset-all");
    expect(source).toContain("emit(\"setVolume\"");
    expect(source).toContain("emit(\"resetAll\")");
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

async function renderVueComponent(path: string, props: Record<string, unknown>): Promise<string> {
  const component = await loadVueComponent(path);
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function loadVueComponent(path: string) {
  const filename = new URL(path, import.meta.url);
  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-volume-mixer-dialog-"));
  try {
    const modulePath = compileVueModule(filename, tempDir, new Map());
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function compileVueModule(filename: URL, tempDir: string, cache: Map<string, string>): string {
  const cached = cache.get(filename.pathname);
  if (cached) return cached;
  const modulePath = join(tempDir, `${cache.size}.mjs`);
  cache.set(filename.pathname, modulePath);

  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: true,
  });
  const compiled = [
    rewriteImports(script.content.replace("export default", "const __sfc__ ="), filename, tempDir, cache),
    "export default __sfc__;",
  ].join("\n");
  const js = ts.transpileModule(compiled, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
      verbatimModuleSyntax: false,
    },
  }).outputText;
  writeFileSync(modulePath, js);
  return modulePath;
}

function rewriteImports(
  code: string,
  importer: URL,
  tempDir: string,
  cache: Map<string, string>,
): string {
  return code
    .replace(/^(\s*import\b(?:(?!;)[\s\S])*?\s+from\s+)["']([^"']+)["']/gm, (
      _match,
      prefix: string,
      specifier: string,
    ) =>
      `${prefix}${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir, cache))}`)
    .replace(/^(\s*export\b(?:(?!;)[\s\S])*?\s+from\s+)["']([^"']+)["']/gm, (
      _match,
      prefix: string,
      specifier: string,
    ) =>
      `${prefix}${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir, cache))}`);
}

function resolveModuleSpecifier(
  specifier: string,
  importer: URL,
  tempDir: string,
  cache: Map<string, string>,
): string {
  if (specifier.startsWith("@/") && specifier.endsWith(".vue")) {
    if (specifier === "@/components/ui/AppDialog.vue") {
      return pathToFileURL(writeAppDialogStub(tempDir)).href;
    }
    return pathToFileURL(compileVueModule(resolveImportUrl(specifier, importer), tempDir, cache)).href;
  }
  if (specifier.endsWith(".vue")) {
    return pathToFileURL(compileVueModule(resolveImportUrl(specifier, importer), tempDir, cache)).href;
  }
  if (specifier.startsWith("@/")) {
    return new URL(`../src/${specifier.slice(2)}`, import.meta.url).href;
  }
  if (specifier.startsWith(".")) {
    return new URL(specifier, importer).href;
  }
  return require.resolve(specifier);
}

function writeAppDialogStub(tempDir: string): string {
  const modulePath = join(tempDir, "AppDialogStub.mjs");
  writeFileSync(
    modulePath,
    [
      `import { defineComponent, h } from ${JSON.stringify(require.resolve("vue"))};`,
      "export default defineComponent({",
      "  props: { open: { type: Boolean, default: false } },",
      "  setup(props, { slots }) {",
      "    return () => (props.open ? h('div', { role: 'dialog' }, slots.default?.()) : null);",
      "  },",
      "});",
    ].join("\n"),
  );
  return modulePath;
}

function resolveImportUrl(specifier: string, importer: URL): URL {
  if (specifier.startsWith("@/")) {
    return new URL(`../src/${specifier.slice(2)}`, import.meta.url);
  }
  return new URL(specifier, importer);
}
