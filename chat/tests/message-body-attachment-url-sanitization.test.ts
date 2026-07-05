import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createSSRApp, h } from "vue";
import { parse, compileScript } from "vue/compiler-sfc";
import { renderToString } from "vue/server-renderer";
import ts from "typescript";
import type { TimelineMessage, TimelineSharedFile } from "../src/lib/chat-ui";

describe("MessageBody attachment URL sanitization", () => {
  test("a javascript: downloadable attachment renders inert — no javascript: href anywhere", async () => {
    const html = await renderMessageBody({
      message: messageWithSharedFiles([
        {
          url: "javascript:alert(1)",
          name: "totally-a-file.bin",
          mediaType: "application/octet-stream",
          disposition: "attachment",
        },
      ]),
    });

    expect(html).not.toContain("javascript:");
    // The file card still renders (inert, without a link target).
    expect(html).toContain("totally-a-file.bin");
    expect(html).not.toContain('href="javascript:alert(1)"');
  });

  test("a javascript: downloadable attachment in compact mode renders inert", async () => {
    const html = await renderMessageBody({
      message: messageWithSharedFiles([
        {
          url: "javascript:alert(1)",
          name: "totally-a-file.bin",
          mediaType: "application/octet-stream",
          disposition: "attachment",
        },
      ]),
      compact: true,
    });

    expect(html).not.toContain("javascript:");
    expect(html).toContain("totally-a-file.bin");
  });

  test("a data:text/html PDF attachment never reaches the iframe src", async () => {
    const html = await renderMessageBody({
      message: messageWithSharedFiles([
        {
          url: "data:text/html,<script>alert(1)</script>",
          name: "evil.pdf",
          mediaType: "application/pdf",
          disposition: "inline",
        },
      ]),
    });

    expect(html).not.toContain("data:text/html");
    expect(html).not.toContain("<iframe");
  });

  test("javascript: inline image, video, and audio attachments render no media src", async () => {
    const html = await renderMessageBody({
      message: messageWithSharedFiles([
        { url: "javascript:alert('img')", mediaType: "image/png", disposition: "inline" },
        { url: "javascript:alert('vid')", mediaType: "video/mp4", disposition: "inline" },
        { url: "javascript:alert('aud')", mediaType: "audio/mpeg", disposition: "inline" },
      ]),
    });

    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("<img");
    expect(html).not.toContain("<video");
    expect(html).not.toContain("<audio");
  });

  test("a javascript: sticker renders no image", async () => {
    const html = await renderMessageBody({
      message: {
        ...messageWithSharedFiles([
          { url: "javascript:alert(1)", mediaType: "image/png", disposition: "inline" },
        ]),
        isSticker: true,
        body: "sticker alt",
      },
    });

    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("<img");
  });

  test("control: a normal https downloadable attachment keeps its href", async () => {
    const html = await renderMessageBody({
      message: messageWithSharedFiles([
        {
          url: "https://files.example.com/report.bin",
          name: "report.bin",
          mediaType: "application/octet-stream",
          disposition: "attachment",
        },
      ]),
    });

    expect(html).toContain('href="https://files.example.com/report.bin"');
    expect(html).toContain("report.bin");
  });

  test("control: a normal https inline image keeps its src", async () => {
    const html = await renderMessageBody({
      message: messageWithSharedFiles([
        {
          url: "https://files.example.com/photo.png",
          name: "photo.png",
          mediaType: "image/png",
          disposition: "inline",
        },
      ]),
    });

    expect(html).toContain('src="https://files.example.com/photo.png"');
  });
});

function messageWithSharedFiles(sharedFiles: TimelineSharedFile[]): TimelineMessage {
  return {
    id: "m1",
    author: "Alice",
    body: "",
    createdAt: "2026-06-02T12:00:00.000Z",
    createdAtSource: "fallback",
    isSelf: false,
    sharedFiles,
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
    id: "message-body-attachment-url-sanitization-test",
    inlineTemplate: true,
  });

  const tempDir = mkdtempSync(join(tmpdir(), "waddle-message-body-attachments-"));
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
