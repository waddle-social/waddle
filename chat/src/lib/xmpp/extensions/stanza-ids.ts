/** XEP-0359: Unique and Stable Stanza IDs. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_SID_0 = "urn:xmpp:sid:0";

export interface WaddleOriginId {
  id: string;
}

export interface WaddleStanzaId {
  id: string;
  by?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.originId", multiple: false }],
    element: "origin-id",
    fields: {
      id: attribute("id"),
    },
    namespace: NS_SID_0,
  },
  {
    aliases: [{ path: "message.stanzaIds", multiple: true }],
    element: "stanza-id",
    fields: {
      id: attribute("id"),
      by: attribute("by"),
    },
    namespace: NS_SID_0,
  },
];

export default definitions;
