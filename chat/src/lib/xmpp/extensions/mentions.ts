/** XEP-0513: Explicit Mentions. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childBoolean } from "stanza/jxt";

const NS_EXPLICIT_MENTIONS_0 = "urn:xmpp:mentions:0";

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.explicitMentions", multiple: true }],
    element: "mention",
    fields: {
      begin: attribute("begin"),
      end: attribute("end"),
      jid: attribute("jid"),
      occupantId: attribute("occupantid"),
      mentions: attribute("mentions"),
      uri: attribute("uri"),
      active: childBoolean(NS_EXPLICIT_MENTIONS_0, "active"),
      noping: childBoolean(NS_EXPLICIT_MENTIONS_0, "noping"),
    },
    namespace: NS_EXPLICIT_MENTIONS_0,
  },
];

export default definitions;
