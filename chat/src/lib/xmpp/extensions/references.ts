/** XEP-0372: References (mentions). */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_REFERENCE_0 = "urn:xmpp:reference:0";

export interface WaddleReference {
  type: string;
  uri: string;
  begin?: string;
  end?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.references", multiple: true }],
    element: "reference",
    fields: {
      type: attribute("type"),
      uri: attribute("uri"),
      begin: attribute("begin"),
      end: attribute("end"),
    },
    namespace: NS_REFERENCE_0,
  },
];

export default definitions;
