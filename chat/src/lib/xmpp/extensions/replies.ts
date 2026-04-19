/** XEP-0461: Message Replies. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_REPLY_0 = "urn:xmpp:reply:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.reply", multiple: false }],
    element: "reply",
    fields: {
      to: attribute("to"),
      id: attribute("id"),
    },
    namespace: NS_REPLY_0,
  },
];

export default definitions;
