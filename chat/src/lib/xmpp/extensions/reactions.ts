/** XEP-0444: Message Reactions. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, multipleChildText } from "stanza/jxt";

const NS_REACTIONS_0 = "urn:xmpp:reactions:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.reactions", multiple: false }],
    element: "reactions",
    fields: {
      id: attribute("id"),
      items: multipleChildText(NS_REACTIONS_0, "reaction"),
    },
    namespace: NS_REACTIONS_0,
  },
];

export default definitions;
