/** XEP-0449: Stickers. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_STICKERS_0 = "urn:xmpp:stickers:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.sticker", multiple: false }],
    element: "sticker",
    fields: { pack: attribute("pack") },
    namespace: NS_STICKERS_0,
  },
];

export default definitions;
