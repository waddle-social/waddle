import type { Component } from "vue";
import type { Editor } from "@tiptap/core";
import { Bold, Italic, Strikethrough, Code, List, ListOrdered, TextQuote, SquareCode } from "lucide-vue-next";

/** One toggleable formatting command shared by the composer's fixed
 * formatting bar and the selection bubble toolbar. */
export interface EditorFormatAction {
  name: string;
  title: string;
  icon: Component;
  run: (editor: Editor) => void;
  isActive: (editor: Editor) => boolean;
}

/** Inline marks, in the order Slack's formatting bar shows them. */
export const INLINE_FORMAT_ACTIONS: readonly EditorFormatAction[] = [
  {
    name: "bold",
    title: "Bold",
    icon: Bold,
    run: (editor) => editor.chain().focus().toggleBold().run(),
    isActive: (editor) => editor.isActive("bold"),
  },
  {
    name: "italic",
    title: "Italic",
    icon: Italic,
    run: (editor) => editor.chain().focus().toggleItalic().run(),
    isActive: (editor) => editor.isActive("italic"),
  },
  {
    name: "strike",
    title: "Strikethrough",
    icon: Strikethrough,
    run: (editor) => editor.chain().focus().toggleStrike().run(),
    isActive: (editor) => editor.isActive("strike"),
  },
];

/** Block-level structure (lists, quote, code). */
export const BLOCK_FORMAT_ACTIONS: readonly EditorFormatAction[] = [
  {
    name: "ordered-list",
    title: "Numbered list",
    icon: ListOrdered,
    run: (editor) => editor.chain().focus().toggleOrderedList().run(),
    isActive: (editor) => editor.isActive("orderedList"),
  },
  {
    name: "bullet-list",
    title: "Bulleted list",
    icon: List,
    run: (editor) => editor.chain().focus().toggleBulletList().run(),
    isActive: (editor) => editor.isActive("bulletList"),
  },
  {
    name: "blockquote",
    title: "Quote",
    icon: TextQuote,
    run: (editor) => editor.chain().focus().toggleBlockquote().run(),
    isActive: (editor) => editor.isActive("blockquote"),
  },
  {
    name: "code",
    title: "Code",
    icon: Code,
    run: (editor) => editor.chain().focus().toggleCode().run(),
    isActive: (editor) => editor.isActive("code"),
  },
  {
    name: "code-block",
    title: "Code block",
    icon: SquareCode,
    run: (editor) => editor.chain().focus().toggleCodeBlock().run(),
    isActive: (editor) => editor.isActive("codeBlock"),
  },
];
