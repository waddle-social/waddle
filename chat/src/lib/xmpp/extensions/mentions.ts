/** XEP-0513: Explicit Mentions. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, splicePath } from "stanza/jxt";

const NS_EXPLICIT_MENTIONS_0 = "urn:xmpp:emn:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.explicitMentions", multiple: false }],
    element: "mentions",
    fields: {
      items: splicePath(NS_EXPLICIT_MENTIONS_0, "mentions", "explicitMention", true),
    },
    namespace: NS_EXPLICIT_MENTIONS_0,
  },
  {
    element: "mention",
    fields: { type: attribute("type") },
    namespace: NS_EXPLICIT_MENTIONS_0,
    path: "explicitMention",
  },
];

export default definitions;
