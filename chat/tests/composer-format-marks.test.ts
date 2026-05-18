import { describe, expect, test } from "bun:test";
import { Editor } from "@tiptap/core";
import { createChatEditorExtensions } from "../src/lib/editor/chat-editor-extensions";

if (typeof globalThis.requestAnimationFrame !== "function") {
  globalThis.requestAnimationFrame = ((callback: FrameRequestCallback) => {
    callback(performance.now());
    return 0;
  }) as typeof globalThis.requestAnimationFrame;
}

function selectWord(editor: Editor, word: string): { from: number; to: number } {
  const text = editor.state.doc.textContent;
  const idx = text.indexOf(word);
  expect(idx).toBeGreaterThanOrEqual(0);
  // Paragraph contents start at position 1 (after the <p> open token).
  const from = idx + 1;
  const to = from + word.length;
  editor.commands.setTextSelection({ from, to });
  return { from, to };
}

function markNames(editor: Editor, pos: number): string[] {
  const $pos = editor.state.doc.resolve(pos);
  const marks = $pos.marks();
  return marks.map((m) => m.type.name);
}

describe("composer formatting marks", () => {
  test("toggleBold wraps the selection in a bold mark", () => {
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: { type: "doc", content: [{ type: "paragraph", content: [{ type: "text", text: "hello world" }] }] },
    });

    const { from } = selectWord(editor, "hello");
    editor.chain().toggleBold().run();

    expect(editor.isActive("bold")).toBe(true);
    expect(markNames(editor, from + 1)).toContain("bold");
  });

  test("toggleItalic and toggleCode produce italic + code marks", () => {
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: { type: "doc", content: [{ type: "paragraph", content: [{ type: "text", text: "alpha beta gamma" }] }] },
    });

    selectWord(editor, "alpha");
    editor.chain().toggleItalic().run();
    expect(editor.isActive("italic")).toBe(true);

    const { from: codeFrom } = selectWord(editor, "gamma");
    editor.chain().toggleCode().run();
    expect(editor.isActive("code")).toBe(true);
    expect(markNames(editor, codeFrom + 1)).toContain("code");
  });

  test("setLink applies a link mark with the canonical href", () => {
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: { type: "doc", content: [{ type: "paragraph", content: [{ type: "text", text: "click here" }] }] },
    });

    const { from } = selectWord(editor, "click");
    editor.chain().setLink({ href: "https://example.com/" }).run();

    expect(editor.isActive("link")).toBe(true);
    const $pos = editor.state.doc.resolve(from + 1);
    const link = $pos.marks().find((m) => m.type.name === "link");
    expect(link?.attrs.href).toBe("https://example.com/");
  });

  test("unsetLink clears the link mark", () => {
    const editor = new Editor({
      enableCoreExtensions: { keymap: false },
      extensions: createChatEditorExtensions({ includePlaceholder: false }),
      content: {
        type: "doc",
        content: [{
          type: "paragraph",
          content: [{
            type: "text",
            text: "click",
            marks: [{ type: "link", attrs: { href: "https://example.com/" } }],
          }],
        }],
      },
    });

    selectWord(editor, "click");
    expect(editor.isActive("link")).toBe(true);

    editor.chain().extendMarkRange("link").unsetLink().run();
    expect(editor.isActive("link")).toBe(false);
  });
});
