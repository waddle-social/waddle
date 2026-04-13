/** XEP-0357: Push Notifications (client IQ enable/disable). */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_PUSH_0 = "urn:xmpp:push:0";

export interface WaddlePushEnable {
  jid: string;
  node?: string;
  endpoint?: string;
  p256dh?: string;
  auth?: string;
}

export interface WaddlePushDisable {
  jid: string;
  node?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "iq.pushEnable", multiple: false }],
    element: "enable",
    fields: {
      jid: attribute("jid"),
      node: attribute("node"),
      endpoint: attribute("endpoint"),
      p256dh: attribute("p256dh"),
      auth: attribute("auth"),
    },
    namespace: NS_PUSH_0,
  },
  {
    aliases: [{ path: "iq.pushDisable", multiple: false }],
    element: "disable",
    fields: {
      jid: attribute("jid"),
      node: attribute("node"),
    },
    namespace: NS_PUSH_0,
  },
];

export default definitions;
