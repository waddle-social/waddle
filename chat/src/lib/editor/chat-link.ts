import Link, { isAllowedUri } from "@tiptap/extension-link";
import type { Slice } from "@tiptap/pm/model";
import { Plugin, PluginKey } from "@tiptap/pm/state";

function sliceTextContent(slice: Slice): string {
  let text = "";
  slice.content.forEach((node) => {
    text += node.textContent;
  });
  return text;
}

function pastedUrl(text: string, defaultProtocol: string): { text: string; href: string } | null {
  const value = text.trim();
  if (!value || /\s/.test(value)) return null;

  const hasProtocol = /^[a-z][a-z0-9+.-]*:\/\//i.test(value);
  const hasMaybeProtocol = /^[a-z][a-z0-9+.-]*:/i.test(value);
  if (hasMaybeProtocol && !hasProtocol) return null;

  const href = hasProtocol ? value : `${defaultProtocol}://${value}`;

  try {
    const url = new URL(href);
    if (!["http:", "https:"].includes(url.protocol)) return null;
    if (!url.hostname.includes(".")) return null;
  } catch {
    return null;
  }

  return { text: value, href };
}

export const ChatLink = Link.extend({
  inclusive() {
    // Keep pasted/autolinked URLs from absorbing text typed at the mark boundary.
    return false;
  },

  addProseMirrorPlugins() {
    return [
      ...(this.parent?.() ?? []),
      new Plugin({
        key: new PluginKey("chatLinkPasteUrl"),
        props: {
          handlePaste: (view, _event, slice) => {
            if (!view.state.selection.empty) return false;

            const link = pastedUrl(sliceTextContent(slice), this.options.defaultProtocol);
            if (!link) return false;
            if (!this.options.shouldAutoLink(link.text)) return false;
            if (!this.options.isAllowedUri(link.href, {
              defaultValidate: (href) => !!isAllowedUri(href, this.options.protocols),
              protocols: this.options.protocols,
              defaultProtocol: this.options.defaultProtocol,
            })) {
              return false;
            }

            const node = view.state.schema.text(link.text, [
              this.type.create({ href: link.href }),
            ]);
            const tr = view.state.tr
              .replaceSelectionWith(node, false)
              .removeStoredMark(this.type)
              .scrollIntoView();
            view.dispatch(tr);
            return true;
          },
        },
      }),
    ];
  },
});
