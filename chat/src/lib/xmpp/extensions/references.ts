/** XEP-0372: References (mentions + server-injected data references with link previews). */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import { attribute } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";
import { importPreview, type WaddleLinkPreview } from "./preview";

const NS_REFERENCE_0 = "urn:xmpp:reference:0";

export interface WaddleReference {
  type: string;
  uri: string;
  begin?: string;
  end?: string;
  /**
   * Server-injected link preview payload. Clients never emit this —
   * the waddle-xmpp-xep-link-preview crate authoritatively attaches
   * it on outbound messages. Malformed/mismatched previews are
   * silently dropped on import (with a further receiver-side
   * anti-spoof check in `message-parsing.ts`).
   */
  preview?: WaddleLinkPreview;
}

const previewField: FieldDefinition<WaddleLinkPreview | undefined> = {
  importer(xml: XMLElement) {
    return importPreview(xml);
  },
  exporter() {
    // Client never emits previews; server is authoritative.
  },
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.references", multiple: true }],
    element: "reference",
    fields: {
      type: attribute("type"),
      uri: attribute("uri"),
      begin: attribute("begin"),
      end: attribute("end"),
      preview: previewField,
    },
    namespace: NS_REFERENCE_0,
  },
];

export default definitions;
