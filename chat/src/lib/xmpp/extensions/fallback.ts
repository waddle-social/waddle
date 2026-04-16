/**
 * XEP-0428: Fallback Indication.
 *
 * Marks the message body as a fallback for a structured element:
 *   <fallback xmlns="urn:xmpp:fallback:0" for="urn:xmpp:sfs:0">
 *     <body/>
 *   </fallback>
 */
import type { DefinitionOptions, FieldDefinition } from "stanza/jxt";
import { attribute } from "stanza/jxt";
import XMLElement from "stanza/jxt/Element";

const NS_FALLBACK = "urn:xmpp:fallback:0";

/** Always exports an empty <body/> child element inside <fallback/>. */
const bodyChild: FieldDefinition<boolean> = {
  importer(xml: XMLElement) {
    return xml.getChild("body") !== undefined;
  },
  exporter(xml: XMLElement, value: boolean) {
    if (value && !xml.getChild("body")) {
      xml.appendChild(new XMLElement("body"));
    }
  },
};

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.fallback", multiple: false }],
    element: "fallback",
    fields: {
      for: attribute("for"),
      body: bodyChild,
    },
    namespace: NS_FALLBACK,
  },
];

export default definitions;
