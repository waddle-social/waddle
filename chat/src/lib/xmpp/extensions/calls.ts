/** XEP-0482: Call Invites + XEP-0483: Online Meetings. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childAttribute, childBoolean } from "stanza/jxt";

const NS_CALL_INVITES_0 = "urn:xmpp:call-invites:0";
const NS_ONLINE_MEETINGS_0 = "urn:xmpp:http:online-meetings:invite:0";

export interface WaddleCallPropose {
  id: string;
  audio?: boolean;
  video?: boolean;
  externalUri?: string;
}

export interface WaddleMeeting {
  type?: string;
  url?: string;
  desc?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.callPropose", multiple: false }],
    element: "propose",
    fields: {
      id: attribute("id"),
      audio: childBoolean(NS_CALL_INVITES_0, "audio"),
      video: childBoolean(NS_CALL_INVITES_0, "video"),
      externalUri: childAttribute(NS_CALL_INVITES_0, "external", "uri"),
    },
    namespace: NS_CALL_INVITES_0,
  },
  {
    aliases: [{ path: "message.meeting", multiple: false }],
    element: "meeting",
    fields: {
      type: attribute("type"),
      url: attribute("url"),
      desc: attribute("desc"),
    },
    namespace: NS_ONLINE_MEETINGS_0,
  },
];

export default definitions;
