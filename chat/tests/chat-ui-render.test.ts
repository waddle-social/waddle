import { describe, expect, test } from "bun:test";
import { renderStyledBody, type MarkupSpan } from "../src/lib/chat-ui";

const encoder = new TextEncoder();

function byteLen(input: string): number {
  return encoder.encode(input).byteLength;
}

describe("renderStyledBody", () => {
  test("renders XEP-0393 bold + inline code without leaking markers", () => {
    const html = renderStyledBody("Hello *again!* and `fn main()`");
    expect(html).toContain("<strong>again!</strong>");
    expect(html).toContain("<code");
    expect(html).not.toContain("*again!*");
  });

  test("renders markdown-style double-asterisk bold", () => {
    const html = renderStyledBody("**ss**");
    expect(html).toContain("<strong>ss</strong>");
  });

  test("renders single-newline messages as visible line breaks", () => {
    const html = renderStyledBody("line one\nline two");
    expect(html).toContain("line one<br>line two");
  });

  test("renders blank-line-separated messages as paragraphs", () => {
    const html = renderStyledBody("line one\n\nline two");
    expect(html).toContain("<p>line one</p>");
    expect(html).toContain("<p>line two</p>");
  });

  test("synthesizes styling from markup-only payloads", () => {
    const body = "Hello world";
    const markup: MarkupSpan[] = [
      { type: "b", start: byteLen("Hello "), end: byteLen(body) },
    ];
    const html = renderStyledBody(body, markup);
    expect(html).toContain("Hello <strong>world</strong>");
  });

  test("prefers body markers when body already contains formatting markers", () => {
    const body = "Hello *again!*";
    const markup: MarkupSpan[] = [
      { type: "b", start: byteLen("Hello "), end: byteLen(body) },
    ];
    const html = renderStyledBody(body, markup);
    expect(html).toContain("<strong>again!</strong>");
    expect(html).not.toContain("*again!*");
  });

  test("preserves fenced code language for downstream highlighting", () => {
    const html = renderStyledBody("```rust\nfn main() {}\n```");
    expect(html).toContain('data-code-block="true"');
    expect(html).toContain('data-language="rust"');
  });

  test("synthesizes inline code from markup-only payloads", () => {
    const body = "fn main() {}";
    const markup: MarkupSpan[] = [
      { type: "code", start: 0, end: byteLen(body) },
    ];
    const html = renderStyledBody(body, markup);
    expect(html).toContain("<code");
    expect(html).toContain("fn main() {}");
  });
});
