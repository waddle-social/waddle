import { nextTick, ref } from "vue";
import type { Editor } from "@tiptap/core";

const ALLOWED_LINK_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);

/** Normalise a typed link target, or `null` when it is empty or unsafe. */
export function sanitizeLinkUrl(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return null;
  try {
    const parsed = new URL(trimmed);
    if (!ALLOWED_LINK_PROTOCOLS.has(parsed.protocol)) return null;
    return parsed.toString();
  } catch {
    return null;
  }
}

/**
 * Inline "edit link" field state for a formatting toolbar: open it seeded
 * with the link under the caret, then apply (set or unset) on Enter.
 */
export function useEditorLinkInput(getEditor: () => Editor | null) {
  const linkUrl = ref("");
  const editingLink = ref(false);
  const linkInputRef = ref<HTMLInputElement | null>(null);

  function openLinkInput() {
    const editor = getEditor();
    if (!editor) return;
    editingLink.value = true;
    const href = editor.getAttributes("link").href;
    linkUrl.value = typeof href === "string" && href ? href : "https://";
    void nextTick(() => linkInputRef.value?.focus());
  }

  function applyLink() {
    const editor = getEditor();
    editingLink.value = false;
    if (!editor) return;
    const href = sanitizeLinkUrl(linkUrl.value);
    const chain = editor.chain().focus().extendMarkRange("link");
    if (href) chain.setLink({ href }).run();
    else chain.unsetLink().run();
  }

  function cancelLinkInput() {
    editingLink.value = false;
    getEditor()?.commands.focus();
  }

  return { linkUrl, editingLink, linkInputRef, openLinkInput, applyLink, cancelLinkInput };
}
