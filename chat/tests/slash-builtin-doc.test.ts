import { describe, expect, test } from "bun:test";
import type { JSONContent } from "@tiptap/core";
import { rewriteBuiltinSendDoc, SHRUG } from "../src/lib/slash-builtin-doc";
import { tiptapToRichMessage } from "../src/lib/rich-message";

function text(value: string, marks?: string[]): JSONContent {
  return marks ? { type: "text", text: value, marks: marks.map((type) => ({ type })) } : { type: "text", text: value };
}

function paragraph(...content: JSONContent[]): JSONContent {
  return content.length > 0 ? { type: "paragraph", content } : { type: "paragraph" };
}

function doc(...content: JSONContent[]): JSONContent {
  return { type: "doc", content };
}

function sent(rewrite: "me" | "shrug", input: JSONContent) {
  return tiptapToRichMessage(rewriteBuiltinSendDoc(rewrite, input));
}

describe("rewriteBuiltinSendDoc: shrug", () => {
  test("bare /shrug sends just the shrug", () => {
    expect(sent("shrug", doc(paragraph(text("/shrug")))).body).toBe(SHRUG);
    expect(sent("shrug", doc(paragraph(text("/shrug ")))).body).toBe(SHRUG);
  });

  test("strips the command and appends the shrug after a space", () => {
    expect(sent("shrug", doc(paragraph(text("/shrug oh well")))).body).toBe(`oh well ${SHRUG}`);
    expect(sent("shrug", doc(paragraph(text("/SHRUG   oh well")))).body).toBe(`oh well ${SHRUG}`);
  });

  test("keeps formatting on the trailing text", () => {
    const message = sent("shrug", doc(paragraph(text("/shrug "), text("really", ["bold"]))));
    expect(message.body).toBe(`really ${SHRUG}`);
    expect(message.markup).toEqual([{ type: "span", start: 0, end: 6, styles: ["strong"] }]);
  });

  test("strips across a text-node boundary inside the command", () => {
    const message = sent("shrug", doc(paragraph(text("/shr"), text("ug fine", ["italic"]))));
    expect(message.body).toBe(`fine ${SHRUG}`);
    expect(message.markup).toEqual([{ type: "span", start: 0, end: 4, styles: ["emphasis"] }]);
  });

  test("appends to the last paragraph and drops an emptied first paragraph", () => {
    expect(sent("shrug", doc(paragraph(text("/shrug")), paragraph(text("second")))).body).toBe(`second ${SHRUG}`);
    expect(sent("shrug", doc(paragraph(text("/shrug first")), paragraph(text("second")), paragraph())).body)
      .toBe(`first\n\nsecond ${SHRUG}`);
  });

  test("adds a new paragraph after a trailing non-paragraph block", () => {
    const code: JSONContent = { type: "codeBlock", content: [text("let x = 1;")] };
    const message = sent("shrug", doc(paragraph(text("/shrug look")), code));
    expect(message.body).toBe(`look\n\nlet x = 1;\n\n${SHRUG}`);
  });

  test("the command's own formatting marks do not leak into the result", () => {
    const message = sent("shrug", doc(paragraph(text("/shrug", ["bold"]), text(" ok"))));
    expect(message.body).toBe(`ok ${SHRUG}`);
    expect(message.markup).toEqual([]);
  });
});

describe("rewriteBuiltinSendDoc: me", () => {
  test("sends the typed body unchanged when already canonical", () => {
    expect(sent("me", doc(paragraph(text("/me waves")))).body).toBe("/me waves");
  });

  test("canonicalizes case and spacing to the exact XEP-0245 prefix", () => {
    expect(sent("me", doc(paragraph(text("/ME   waves")))).body).toBe("/me waves");
  });

  test("keeps markup on the action, offset past the /me prefix", () => {
    const message = sent("me", doc(paragraph(text("/me "), text("waves", ["italic"]))));
    expect(message.body).toBe("/me waves");
    expect(message.markup).toEqual([{ type: "span", start: 4, end: 9, styles: ["emphasis"] }]);
  });

  test("keeps later paragraphs", () => {
    expect(sent("me", doc(paragraph(text("/me waves")), paragraph(text("hi all")))).body).toBe("/me waves\n\nhi all");
  });
});
