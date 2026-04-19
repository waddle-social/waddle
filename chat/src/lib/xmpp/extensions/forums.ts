/** XEP-0508: MUC Forums. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute } from "stanza/jxt";

const NS_FORUMS_0 = "urn:xmpp:forums:0";

export interface WaddleThreadCreate {
  title: string;
}

export interface WaddleThreadReply {
  threadId: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.threadCreate", multiple: false }],
    element: "thread-create",
    fields: {
      title: attribute("title"),
    },
    namespace: NS_FORUMS_0,
  },
  {
    aliases: [{ path: "message.threadReply", multiple: false }],
    element: "thread-reply",
    fields: {
      threadId: attribute("thread-id"),
    },
    namespace: NS_FORUMS_0,
  },
];

export default definitions;
