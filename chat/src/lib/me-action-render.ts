import type { JSONContent } from "@tiptap/core";
import { ME_COMMAND_PREFIX_LENGTH } from "./me-command";
import { dropLeadingDocText } from "./rich-message/leading-text";
import { richMessageToTiptap } from "./rich-message/parse";
import { ME_ACTOR_NODE_TYPE, renderRichDocHtml } from "./rich-message/render";
import type { MarkupSpan, MessageReference } from "./rich-message/types";

function withActor(doc: JSONContent, actor: string): JSONContent {
  const actorNode: JSONContent = { type: ME_ACTOR_NODE_TYPE, text: `* ${actor}` };
  const [first, ...rest] = doc.content ?? [];
  if (first?.type !== "paragraph") {
    return { ...doc, content: [{ type: "paragraph", content: [actorNode] }, ...(first ? [first] : []), ...rest] };
  }
  const action = first.content ?? [];
  const lead = action.length > 0 ? [actorNode, { type: "text", text: " " }] : [actorNode];
  return { ...doc, content: [{ ...first, content: [...lead, ...action] }, ...rest] };
}

/**
 * HTML for an XEP-0245 `/me` body: "* Actor action". The body is parsed in
 * full first, so XEP-0394 markup and XEP-0372 reference offsets (code
 * points into the whole body) stay valid; only then is the four-character
 * "/me " prefix dropped from the rendered document and the actor prepended.
 */
export function renderMeActionHtml(input: {
  body: string;
  markup?: readonly MarkupSpan[];
  references?: readonly MessageReference[];
  actor: string;
}): string {
  const parsed = richMessageToTiptap({ body: input.body, markup: input.markup, references: input.references });
  const action = dropLeadingDocText(parsed, ME_COMMAND_PREFIX_LENGTH);
  return renderRichDocHtml(withActor(action, input.actor));
}
