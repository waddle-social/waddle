import type { Editor } from "@tiptap/vue-3";

type EnterLikeEvent = Pick<
  KeyboardEvent,
  "key" | "shiftKey" | "ctrlKey" | "metaKey" | "altKey" | "isComposing"
> & {
  keyCode?: number;
};

const NATIVE_ENTER_NODES = ["blockquote", "codeBlock", "listItem"] as const;

export function shouldSendOnEnter(editor: Editor, event: EnterLikeEvent): boolean {
  if (event.key !== "Enter") return false;
  if (event.shiftKey || event.ctrlKey || event.metaKey || event.altKey) return false;
  if (event.isComposing || event.keyCode === 229) return false;
  if (!editor.isEditable || editor.isEmpty) return false;
  return !NATIVE_ENTER_NODES.some((nodeName) => editor.isActive(nodeName));
}
