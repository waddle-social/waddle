import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import type { CallAnchorCardState } from "../src/lib/call-thread-anchor";

const liveState: CallAnchorCardState = {
  status: "live",
  media: { audio: true, video: true },
  participantCount: 2,
  participantLabels: ["alice", "bob"],
  messageCount: 7,
  threadId: "call-thread-uuid",
  title: "Live video call",
  actionLabel: "Join",
  actionDisabled: false,
  ariaLabel: "Join live video call, 2 people: alice, bob",
};

describe("CallAnchorCard", () => {
  test("renders the live call anchor with media, participants, join, and call-chat count", async () => {
    const html = await renderVueComponent("../src/components/calls/CallAnchorCard.vue", {
      state: liveState,
    });

    expect(html).toContain("Live video call");
    expect(html).toContain("call-anchor-card__pulse");
    expect(html).toContain("alice");
    expect(html).toContain("bob");
    expect(html).toContain("Join");
    expect(html).toContain("7 messages in call chat");
    expect(html).toContain('aria-label="Join live video call, 2 people: alice, bob"');
  });

  test("renders ended call anchors muted without a join action", async () => {
    const html = await renderVueComponent("../src/components/calls/CallAnchorCard.vue", {
      state: {
        ...liveState,
        status: "ended",
        participantCount: 0,
        participantLabels: [],
        title: "Call ended",
        actionLabel: null,
        actionDisabled: false,
        ariaLabel: "Call ended · 5m",
      } satisfies CallAnchorCardState,
    });

    expect(html).toContain("Call ended");
    expect(html).toContain("call-anchor-card--ended");
    expect(html).not.toContain(">Join<");
    expect(html).toContain('aria-label="Call ended · 5m"');
  });
});

async function renderVueComponent(path: string, props: Record<string, unknown>) {
  const component = await loadVueComponent(path);
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

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-vue-component-"));
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
    `${basePath}.js`,
    `${basePath}.vue`,
    `${basePath}/index.ts`,
  ];
  const resolved = candidates.find((candidate) => existsSync(candidate));
  if (!resolved) throw new Error(`Unable to resolve test SFC import: ${basePath}`);
  return resolved;
}

function moduleUrlForPath(resolvedPath: string): string {
  if (!resolvedPath.endsWith(".vue")) return pathToFileURL(resolvedPath).href;
  return new URL("./helpers/vue-sfc-stub.ts", import.meta.url).href;
}
