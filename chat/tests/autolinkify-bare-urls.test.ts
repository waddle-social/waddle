import { describe, expect, test } from "bun:test";
import type { JSONContent } from "@tiptap/core";
import { tiptapToRichMessage } from "@/lib/rich-message";

function paragraph(...content: JSONContent[]): JSONContent {
  return { type: "paragraph", content };
}

function text(text: string, marks?: JSONContent["marks"]): JSONContent {
  return { type: "text", text, ...(marks ? { marks } : {}) };
}

function doc(...content: JSONContent[]): JSONContent {
  return { type: "doc", content };
}

describe("tiptapToRichMessage / autolinkifyBareUrls", () => {
  test("bare URL in plain paragraph emits a type=data reference", () => {
    const result = tiptapToRichMessage(doc(paragraph(text("see https://example.com today"))));

    expect(result.references).toEqual([
      { type: "data", uri: "https://example.com/", begin: 4, end: 23 },
    ]);
  });

  test("URL already wrapped in a TipTap link mark produces exactly one reference", () => {
    const result = tiptapToRichMessage(doc(paragraph(
      text("see "),
      text("https://example.com", [{ type: "link", attrs: { href: "https://example.com" } }]),
    )));

    expect(result.references).toEqual([
      { type: "data", uri: "https://example.com/", begin: 4, end: 23 },
    ]);
  });

  test("URL inside an inline code mark is not auto-linkified", () => {
    const result = tiptapToRichMessage(doc(paragraph(
      text("inline "),
      text("https://example.com", [{ type: "code" }]),
    )));

    expect(result.references).toEqual([]);
  });

  test("URL inside a fenced code block is not auto-linkified", () => {
    const result = tiptapToRichMessage(doc({
      type: "codeBlock",
      content: [text("see https://example.com")],
    }));

    expect(result.references).toEqual([]);
  });

  test("offsets count Unicode scalar values, not UTF-16 code units", () => {
    // `📎 ` is one scalar value (counted as 1 by codePointLength) but two
    // UTF-16 code units (counted as 2 by `match.index`). The reference must
    // begin at scalar offset 2 (`📎`, ` `), not 3.
    const result = tiptapToRichMessage(doc(paragraph(text("📎 https://example.com"))));

    expect(result.references).toEqual([
      { type: "data", uri: "https://example.com/", begin: 2, end: 21 },
    ]);
  });

  test("multiple URLs in one message produce multiple non-overlapping references in body order", () => {
    const result = tiptapToRichMessage(doc(paragraph(
      text("a https://one.example b https://two.example c"),
    )));

    expect(result.references).toHaveLength(2);
    expect(result.references[0]).toMatchObject({ uri: "https://one.example/", begin: 2, end: 21 });
    expect(result.references[1]).toMatchObject({ uri: "https://two.example/", begin: 24, end: 43 });
  });

  test("trailing sentence punctuation is stripped from the URL", () => {
    const result = tiptapToRichMessage(doc(paragraph(text("see https://example.com."))));

    expect(result.references).toEqual([
      { type: "data", uri: "https://example.com/", begin: 4, end: 23 },
    ]);
  });

  test("non-http URLs (mailto, ftp, javascript) are not auto-linkified", () => {
    // The regex only matches http(s); other schemes never enter the pipeline.
    // safeUri also rejects javascript: as a defense in depth.
    const result = tiptapToRichMessage(doc(paragraph(
      text("ping me at mailto:foo@example.com or javascript:alert(1)"),
    )));

    expect(result.references).toEqual([]);
  });
});
