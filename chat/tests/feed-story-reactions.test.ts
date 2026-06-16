import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";

const FEED_PANE = "../src/components/community/FeedPane.vue";

function feedPaneProps(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    entries: [],
    stories: [],
    isLoading: false,
    isStoriesLoading: false,
    isPosting: false,
    isStoryPosting: false,
    error: null,
    storiesError: null,
    canPost: true,
    selfJid: "me@example.com",
    ...overrides,
  };
}

describe("Community feed story reactions", () => {
  test("reactToStory emits react with the targeted story id and emoji", async () => {
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent(FEED_PANE, feedPaneProps(), (...args) => {
      emitted.push(args);
    });

    setupBindingFunction(bindings, "reactToStory")("story-1", "👍");

    expect(emitted).toEqual([["react", "story-1", "👍"]]);
  });

  test("renders a react button on each story card in the feed", async () => {
    const html = await renderVueComponent(FEED_PANE, feedPaneProps({
      stories: [{ id: "story-1", author: "alice@example.com", body: "hello" }],
    }));

    expect(html).toContain('aria-label="React to story"');
  });

  test("renders clickable reaction count chips, highlighting the user's own reactions", async () => {
    const html = await renderVueComponent(FEED_PANE, feedPaneProps({
      stories: [{ id: "story-1", author: "alice@example.com", body: "hi" }],
      reactionSummary: (id: string) =>
        id === "story-1"
          ? {
              counts: { "👍": 2, "❤️": 1 },
              reactors: { "👍": ["a@x", "me@example.com"], "❤️": ["b@x"] },
              mine: ["👍"],
            }
          : { counts: {}, reactors: {}, mine: [] },
    }));

    const thumbChip = html.match(/<button[^>]*React to story with 👍[\s\S]*?<\/button>/)?.[0] ?? "";
    expect(thumbChip).toContain('aria-pressed="true"');
    expect(thumbChip).toContain("2");

    const heartChip = html.match(/<button[^>]*React to story with ❤️[\s\S]*?<\/button>/)?.[0] ?? "";
    expect(heartChip).toContain('aria-pressed="false"');
    expect(heartChip).toContain("1");
  });

  test("shows the react affordance on the viewer's own story (self-react parity)", async () => {
    const html = await renderVueComponent(FEED_PANE, feedPaneProps({
      selfJid: "me@example.com",
      stories: [{ id: "mine-1", author: "me@example.com", body: "my story" }],
      reactionSummary: (id: string) =>
        id === "mine-1"
          ? { counts: { "🎉": 1 }, reactors: { "🎉": ["me@example.com"] }, mine: ["🎉"] }
          : { counts: {}, reactors: {}, mine: [] },
    }));

    expect(html).toContain('aria-label="React to story"');
    const ownChip = html.match(/<button[^>]*React to story with 🎉[\s\S]*?<\/button>/)?.[0] ?? "";
    expect(ownChip).toContain('aria-pressed="true"');
  });

  test("opening a story from the feed still emits storySelected after de-nesting the card", async () => {
    const emitted: unknown[][] = [];
    const bindings = await setupVueComponent(
      FEED_PANE,
      feedPaneProps({ stories: [{ id: "story-1", author: "alice@example.com" }] }),
      (...args) => {
        emitted.push(args);
      },
    );

    setupBindingFunction(bindings, "selectStory")(0);

    expect(emitted).toContainEqual(["storySelected", "story-1"]);
  });

  test("does not render a react button on non-story feed entries", async () => {
    const html = await renderVueComponent(FEED_PANE, feedPaneProps({
      entries: [{ id: "post-1", author: "bob@example.com", body: "a community post", publishedMs: 1000 }],
      stories: [],
    }));

    expect(html).toContain("a community post");
    expect(html).not.toContain('aria-label="React to story"');
  });
});

async function renderVueComponent(path: string, props: Record<string, unknown>) {
  const component = await loadVueComponent(path);
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function setupVueComponent(
  path: string,
  props: Record<string, unknown>,
  emit: (...args: unknown[]) => void = () => undefined,
): Promise<Record<string, unknown>> {
  const component = await loadVueComponent(path, { inlineTemplate: false }) as {
    setup?: (
      props: Record<string, unknown>,
      context: {
        emit: (...args: unknown[]) => void;
        expose: () => void;
        attrs: Record<string, unknown>;
        slots: Record<string, unknown>;
      },
    ) => Record<string, unknown>;
  };
  if (!component.setup) throw new Error(`${path} has no setup function`);
  return component.setup(props, {
    emit,
    expose: () => undefined,
    attrs: {},
    slots: {},
  });
}

function setupBindingFunction(
  bindings: Record<string, unknown>,
  key: string,
): (...args: unknown[]) => unknown {
  const binding = bindings[key];
  if (typeof binding !== "function") {
    throw new Error(`Expected setup binding ${key} to be a function`);
  }
  return binding as (...args: unknown[]) => unknown;
}

async function loadVueComponent(path: string, options: { inlineTemplate?: boolean } = {}) {
  const filename = new URL(path, import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: filename.pathname,
    inlineTemplate: options.inlineTemplate ?? true,
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
  if (!resolvedPath.endsWith(".vue")) return pathToFileURL(resolvedPath).href;
  return new URL("./helpers/vue-sfc-stub.ts", import.meta.url).href;
}
