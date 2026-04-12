/** XEP-0424: Message Retraction + XEP-0425: Message Moderation. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childBoolean, childText } from "stanza/jxt";

const NS_MESSAGE_RETRACT_1 = "urn:xmpp:message-retract:1";
const NS_MESSAGE_MODERATE_1 = "urn:xmpp:message-moderate:1";
const NS_FASTEN_0 = "urn:xmpp:fasten:0";

export interface WaddleRetract {
  id: string;
}

export interface WaddleApplyTo {
  id: string;
  moderated?: {
    retract?: boolean;
    reason?: string;
  };
}

const definitions: DefinitionOptions[] = [
  // XEP-0424: <retract id="msg-id"/>
  {
    aliases: [{ path: "message.retract", multiple: false }],
    element: "retract",
    fields: { id: attribute("id") },
    namespace: NS_MESSAGE_RETRACT_1,
  },

  // XEP-0425: <apply-to id="target"><moderated>...</moderated></apply-to>
  {
    aliases: [{ path: "message.applyTo", multiple: false }],
    element: "apply-to",
    fields: { id: attribute("id") },
    namespace: NS_FASTEN_0,
  },
  {
    aliases: [{ path: "message.applyTo.moderated", multiple: false }],
    element: "moderated",
    fields: {
      retract: childBoolean(NS_MESSAGE_RETRACT_1, "retract"),
      reason: childText(NS_MESSAGE_MODERATE_1, "reason"),
    },
    namespace: NS_MESSAGE_MODERATE_1,
    path: "moderated",
  },
];

export default definitions;
