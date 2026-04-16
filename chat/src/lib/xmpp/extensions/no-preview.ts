/**
 * Sender hint: when present as a direct child of a `<message>`, the
 * server's link-preview enricher MUST skip enrichment for that message.
 * Used to implement the composer's per-message "Preview link: off"
 * toggle.
 *
 * Wire format:
 *   <message type='groupchat'>
 *     <body>secret https://example.com/foo</body>
 *     <no-preview xmlns='urn:waddle:link-preview:0'/>
 *   </message>
 *
 * The element carries no children or attributes — its presence is the
 * whole signal. Receiver doesn't need to parse it; it matters only to
 * the server. On the sender side, set `msg.suppressLinkPreview = {}`
 * when building the outbound message.
 */
import type { DefinitionOptions } from "stanza/jxt";

const NS_WADDLE_PREVIEW_0 = "urn:waddle:link-preview:0";

export interface WaddleSuppressLinkPreview {
  // presence-only marker; no attributes or text
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.suppressLinkPreview", multiple: false }],
    element: "no-preview",
    fields: {},
    namespace: NS_WADDLE_PREVIEW_0,
  },
];

export default definitions;
