import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import type { TimelineMessage } from "../src/lib/chat-ui";

describe("MessageBody link previews", () => {
  test("renders cached preview images and unavailable remote media states", async () => {
    const html = await renderMessageBody({
      message: messageWithPreviews([
        {
          originalUrl: "https://example.com/article",
          title: "Cached article",
          description: "A preview with trusted cached media",
          image: {
            url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview.png",
            mediaType: "image/png",
            width: 640,
            height: 360,
            alt: "Article preview",
          },
          remoteMediaUnavailable: true,
        },
        {
          originalUrl: "https://remote.example/post",
          title: "Remote article",
          remoteMediaUnavailable: true,
        },
      ]),
    });

    expect(html).toContain('src="https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview.png"');
    expect(html).toContain('alt="Article preview"');
    expect(html).toContain('width="640"');
    expect(html).toContain('height="360"');
    expect(html).toContain("Cached article");
    expect(html).toContain("Remote preview media unavailable");
    expect(html).toContain("Remote article");
  });

  test("renders a direct-video preview as an accessible play control without preloading the video", async () => {
    const html = await renderMessageBody({
      message: messageWithPreviews([
        {
          originalUrl: "https://cdn.example.com/clip.mp4",
          normalizedUrl: "https://cdn.example.com/clip.mp4",
          title: "A short clip",
          video: { url: "https://cdn.example.com/clip.mp4", mediaType: "video/mp4", size: 4096 },
        },
      ]),
    });

    // Accessible play control exists.
    expect(html).toContain("aria-label=\"Play video: A short clip\"");
    // Playback starts only after user action: the <video> element (and its
    // network-triggering src) MUST NOT be present in the initial render.
    expect(html).not.toContain("<video");
    expect(html).not.toContain("src=\"https://cdn.example.com/clip.mp4\"");
    expect(html).toContain("A short clip");
  });

  test("renders the cached poster image for a direct-video preview when available", async () => {
    const html = await renderMessageBody({
      message: messageWithPreviews([
        {
          originalUrl: "https://cdn.example.com/clip.mp4",
          normalizedUrl: "https://cdn.example.com/clip.mp4",
          title: "Clip with poster",
          video: { url: "https://cdn.example.com/clip.mp4", mediaType: "video/mp4" },
          image: {
            url: "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview.png",
            mediaType: "image/png",
            alt: "Poster frame",
          },
        },
      ]),
    });

    expect(html).toContain('src="https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview.png"');
    expect(html).toContain("aria-label=\"Play video: Clip with poster\"");
  });

  test("renders a player embed as a play control without loading the iframe", async () => {
    const html = await renderMessageBody({
      message: messageWithPreviews([
        {
          originalUrl: "https://www.youtube.com/watch?v=429A_VugWW0",
          normalizedUrl: "https://www.youtube.com/watch?v=429A_VugWW0",
          title: "A video",
          image: { url: "https://waddle.example/api/files/x.png", mediaType: "image/png" },
          playerEmbed: { url: "https://www.youtube-nocookie.com/embed/429A_VugWW0", width: 1280, height: 720 },
        },
      ]),
    });
    expect(html).toContain('aria-label="Play video: A video"');
    expect(html).not.toContain("<iframe");
    expect(html).not.toContain("youtube-nocookie.com/embed/429A_VugWW0");
    expect(html).toContain("A video");
  });

  test("does not render a player card for a non-allowlisted embed", async () => {
    const html = await renderMessageBody({
      message: messageWithPreviews([
        {
          originalUrl: "https://example.com/x",
          title: "Bad",
          playerEmbed: { url: "https://evil.example.com/embed/x" },
        },
      ]),
    });
    expect(html).not.toContain('aria-label="Play video');
    expect(html).not.toContain("<iframe");
  });

  test("escapes markup in a direct-video preview title — no arbitrary HTML/JS execution", async () => {
    const html = await renderMessageBody({
      message: messageWithPreviews([
        {
          originalUrl: "https://cdn.example.com/clip.mp4",
          normalizedUrl: "https://cdn.example.com/clip.mp4",
          title: "<script>alert('x')</script>",
          video: { url: "https://cdn.example.com/clip.mp4", mediaType: "video/mp4" },
        },
      ]),
    });

    expect(html).not.toContain("<script>alert('x')</script>");
    expect(html).toContain("&lt;script&gt;");
  });
});

function messageWithPreviews(linkPreviews: TimelineMessage["linkPreviews"]): TimelineMessage {
  return {
    id: "m1",
    author: "Alice",
    body: "",
    createdAt: "2026-06-02T12:00:00.000Z",
    createdAtSource: "fallback",
    isSelf: false,
    linkPreviews,
  };
}

async function renderMessageBody(props: { message: TimelineMessage; compact?: boolean }) {
  const component = await loadMessageBodyComponent();
  return renderToString(createSSRApp({ render: () => h(component, props) }));
}

async function loadMessageBodyComponent() {
  const filename = new URL("../src/components/chat/MessageBody.vue", import.meta.url);
  const source = readFileSync(filename, "utf8");
  const { descriptor } = parse(source, { filename: filename.pathname });
  const script = compileScript(descriptor, {
    id: "message-body-link-previews-test",
    inlineTemplate: true,
  });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-message-body-"));
  try {
    const compiled = [
      rewriteImports(script.content.replace("export default", "const __sfc__ ="), filename, tempDir),
      "export default __sfc__;",
    ].join("\n");
    const js = ts.transpileModule(compiled, {
      compilerOptions: {
        module: ts.ModuleKind.ESNext,
        target: ts.ScriptTarget.ES2022,
        verbatimModuleSyntax: false,
      },
    }).outputText;
    const modulePath = join(tempDir, "MessageBody.mjs");
    writeFileSync(modulePath, js);
    const component = await import(pathToFileURL(modulePath).href);
    return component.default;
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function rewriteImports(code: string, importer: URL, tempDir: string): string {
  return code.replace(/from\s+["']([^"']+)["']/g, (_match, specifier: string) =>
    `from ${JSON.stringify(resolveModuleSpecifier(specifier, importer, tempDir))}`);
}

function resolveModuleSpecifier(specifier: string, importer: URL, tempDir: string): string {
  if (specifier.startsWith("@/")) {
    return moduleUrlForPath(resolveSourcePath(new URL(`../src/${specifier.slice(2)}`, import.meta.url).pathname), tempDir);
  }
  if (specifier.startsWith(".")) {
    return moduleUrlForPath(resolveSourcePath(new URL(specifier, importer).pathname), tempDir);
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

function moduleUrlForPath(resolvedPath: string, tempDir: string): string {
  if (!resolvedPath.endsWith(".vue")) return pathToFileURL(resolvedPath).href;
  const stubPath = join(tempDir, `${resolvedPath.replace(/[^a-z0-9]/gi, "_")}.mjs`);
  writeFileSync(stubPath, [
    `import { h } from ${JSON.stringify(import.meta.resolve("vue"))};`,
    "export default { name: 'MessageBodyChildStub', setup(_, { slots }) { return () => h('span', { 'data-vue-stub': 'true' }, slots.default?.()); } };",
  ].join("\n"));
  return pathToFileURL(stubPath).href;
}
