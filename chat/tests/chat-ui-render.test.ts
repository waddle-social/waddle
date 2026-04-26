import { describe, expect, test } from "bun:test";
import {
  inferredFileDisposition,
  githubEmbedDisplayTitle,
  githubEmbedKindLabel,
  githubEmbedNumber,
  isAudioFile,
  isImageFile,
  isPdfFile,
  isVideoFile,
  renderStyledBody,
  type MarkupSpan,
  type MessageReference,
} from "../src/lib/chat-ui";

describe("renderStyledBody", () => {
  test("renders plain text literally instead of parsing Markdown", () => {
    expect(renderStyledBody("Hello **again**")).toBe("<p>Hello **again**</p>");
    expect(renderStyledBody("# heading")).toBe("<p># heading</p>");
    expect(renderStyledBody("![alt](https://example.com/image.png)")).toBe("<p>![alt](https://example.com/image.png)</p>");
  });

  test("renders inline XEP-0394 spans with code point offsets", () => {
    const body = "Hi 👋 world";
    const markup: MarkupSpan[] = [
      { type: "span", start: 5, end: 10, styles: ["strong"] },
      { type: "span", start: 3, end: 4, styles: ["code"] },
    ];

    const html = renderStyledBody(body, markup);

    expect(html).toContain("Hi <code");
    expect(html).toContain("<strong>world</strong>");
  });

  test("renders code blocks, blockquotes, and lists from markup metadata", () => {
    const code = renderStyledBody("const x = 1", [
      { type: "bcode", start: 0, end: 11, language: "ts" },
    ]);
    const quote = renderStyledBody("> quoted", [
      { type: "bquote", start: 0, end: 8 },
    ]);
    const list = renderStyledBody("- one\n- two", [
      { type: "list", start: 0, end: 11, ordered: false, items: [0, 6] },
    ]);

    expect(code).toContain('data-code-block="true"');
    expect(code).toContain('data-language="ts"');
    expect(quote).toContain("<blockquote");
    expect(quote).toContain("<p>quoted</p>");
    expect(list).toContain("<ul>");
    expect(list).toContain("<li><p>one</p></li>");
    expect(list).toContain("<li><p>two</p></li>");
    expect(list).not.toContain("- one");
  });

  test("renders ordered lists and links through XEP-0372 references", () => {
    const body = "3. docs\n4. more";
    const markup: MarkupSpan[] = [
      { type: "list", start: 0, end: body.length, ordered: true, items: [0, 8] },
    ];
    const references: MessageReference[] = [
      { type: "data", uri: "https://example.com/docs", begin: 3, end: 7 },
    ];

    const html = renderStyledBody(body, markup, references);

    expect(html).toContain('<ol start="3">');
    expect(html).toContain('<a href="https://example.com/docs"');
    expect(html).toContain(">docs</a>");
  });

  test("escapes unsafe HTML and rejects unsafe links", () => {
    const html = renderStyledBody("<script>x</script>", undefined, [
      { type: "data", uri: "javascript:alert(1)", begin: 0, end: 8 },
    ]);

    expect(html).toBe("<p>&lt;script&gt;x&lt;/script&gt;</p>");
    expect(html).not.toContain("javascript:");
  });

  test("identifies image attachments from media type or URL", () => {
    expect(isImageFile("image/jpeg")).toBe(true);
    expect(isImageFile(undefined, "https://cdn.example.com/cat.PNG?token=1")).toBe(true);
    expect(isImageFile(undefined, "https://media2.giphy.com/media/abc123/200w")).toBe(true);
    expect(isImageFile("application/octet-stream", "https://cdn.example.com/archive.bin")).toBe(false);
  });

  test("identifies video, audio, and PDF attachments and infers disposition", () => {
    expect(isVideoFile("video/mp4")).toBe(true);
    expect(isVideoFile(undefined, "clip.webm")).toBe(true);
    expect(isAudioFile("audio/mpeg")).toBe(true);
    expect(isAudioFile(undefined, "https://cdn.example.com/theme.ogg")).toBe(true);
    expect(isPdfFile("application/pdf")).toBe(true);
    expect(isPdfFile(undefined, "notes.pdf")).toBe(true);
    expect(inferredFileDisposition("text/plain", "notes.txt")).toBe("attachment");
    expect(inferredFileDisposition("video/mp4", "clip.mp4")).toBe("inline");
  });

  test("formats GitHub embed card metadata", () => {
    const issue = {
      kind: "issue" as const,
      url: "https://github.com/waddle-social/waddle/issues/42",
      owner: "waddle-social",
      name: "waddle",
    };
    const repo = {
      kind: "repo" as const,
      url: "https://github.com/waddle-social/waddle",
      owner: "waddle-social",
      name: "waddle",
    };

    expect(githubEmbedKindLabel(issue.kind)).toBe("Issue");
    expect(githubEmbedNumber(issue)).toBe("42");
    expect(githubEmbedDisplayTitle(issue)).toBe("waddle-social/waddle #42");
    expect(githubEmbedKindLabel(repo.kind)).toBe("Repository");
    expect(githubEmbedNumber(repo)).toBeNull();
    expect(githubEmbedDisplayTitle(repo)).toBe("waddle-social/waddle");
  });
});
