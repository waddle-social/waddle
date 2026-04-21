import { describe, expect, test } from "bun:test";
import { renderStyledBody, type MarkupSpan, type MessageReference } from "../src/lib/chat-ui";

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
});
