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

const require = createRequire(import.meta.url);

describe("CallSettingsDialog speaker support", () => {
  test("renders the graceful disabled speaker state when Web Audio sink routing is unavailable", async () => {
    const html = await renderVueComponent("../src/components/calls/CallSettingsDialog.vue", {
      open: true,
      "onUpdate:open": () => undefined,
    });

    expect(html).toContain("Your browser doesn&#39;t support choosing the speaker device");
    expect(html).not.toContain('aria-label="Speaker"');
    expect(html).not.toContain("System default");
  });

});

describe("CallSettingsDialog audio processing controls", () => {
  test("renders requested noise cancellation controls near the applied-state readout", async () => {
    const html = await renderVueComponent("../src/components/calls/CallSettingsDialog.vue", {
      open: true,
      "onUpdate:open": () => undefined,
    });

    expect(html).toContain("Noise cancellation");
    expect(html).toContain("Echo cancellation");
    expect(html).toContain("Auto gain control");
    expect(html).toContain("<fieldset");
    expect(html).toContain("disabled");
    expect(html).toContain("aria-describedby=\"call-processing-no-mic\"");
    expect(html).toContain("Requested audio processing");
    expect(html).toContain("Audio processing");
  });
});

describe("CallSettingsDialog virtual background controls", () => {
  test("renders opt-in camera background controls", async () => {
    const html = await renderVueComponent("../src/components/calls/CallSettingsDialog.vue", {
      open: true,
      "onUpdate:open": () => undefined,
    });

    expect(html).toContain("Virtual background");
    expect(html).toContain("Background blur");
    expect(html).toContain("Image replacement");
    expect(html).toContain('type="file"');
  });
});

async function renderVueComponent(path: string, props: Record<string, unknown>): Promise<string> {
  const component = await loadVueComponent(path);
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function loadVueComponent(path: string) {
  const filename = new URL(path, import.meta.url);
  const tempDir = mkdtempSync(join(tmpdir(), "waddle-call-settings-dialog-"));
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
      `${prefix}${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir, cache))}`)
    .replace(/^(\s*import\s*\(\s*)["']([^"']+)["'](\s*\))/gm, (
      _match,
      prefix: string,
      specifier: string,
      suffix: string,
    ) =>
      `${prefix}${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir, cache))}${suffix}`)
    .replace(/^(\s*const\s+\w+\s*=\s*await\s+import\s*\(\s*)["']([^"']+)["'](\s*\))/gm, (
      _match,
      prefix: string,
      specifier: string,
      suffix: string,
    ) =>
      `${prefix}${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir, cache))}${suffix}`);
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
      "  setup(_props, { slots }) {",
      "    return () => h('div', { role: 'dialog' }, slots.default?.());",
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
