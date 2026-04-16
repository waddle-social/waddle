/**
 * XEP-0428: Fallback Indication.
 *
 * Marks the message body as a fallback for a structured element:
 *   <fallback xmlns="urn:xmpp:fallback:0" for="urn:xmpp:sfs:0">
 *     <body/>
 *   </fallback>
 */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_FALLBACK = "urn:xmpp:fallback:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.fallback", multiple: false }],
    element: "fallback",
    fields: {
      for: attribute("for"),
    },
    namespace: NS_FALLBACK,
  },
];

export default definitions;
