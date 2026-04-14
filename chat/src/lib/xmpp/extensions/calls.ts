/** XEP-0482: Call Invites + XEP-0483: Online Meetings. */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childAttribute, childBoolean } from "stanza/jxt";

const NS_CALL_INVITES_0 = "urn:xmpp:call-invites:0";
const NS_ONLINE_MEETINGS_0 = "urn:xmpp:http:online-meetings:invite:0";

export interface WaddleCallInvite {
  id?: string;
  muji?: boolean;
  jingleSid?: string;
  jingleJid?: string;
  externalUri?: string;
}

export interface WaddleMeeting {
  type?: string;
  url?: string;
  desc?: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "message.callInvite", multiple: false }],
    element: "invite",
    fields: {
      id: attribute("id"),
      muji: childBoolean(NS_CALL_INVITES_0, "muji"),
      jingleSid: childAttribute(NS_CALL_INVITES_0, "jingle", "sid"),
      jingleJid: childAttribute(NS_CALL_INVITES_0, "jingle", "jid"),
      externalUri: childAttribute(NS_CALL_INVITES_0, "external", "uri"),
    },
    namespace: NS_CALL_INVITES_0,
  },
  {
    aliases: [{ path: "message.callAccept", multiple: false }],
    element: "accept",
    fields: {
      id: attribute("id"),
      muji: childBoolean(NS_CALL_INVITES_0, "muji"),
      jingleSid: childAttribute(NS_CALL_INVITES_0, "jingle", "sid"),
      jingleJid: childAttribute(NS_CALL_INVITES_0, "jingle", "jid"),
      externalUri: childAttribute(NS_CALL_INVITES_0, "external", "uri"),
    },
    namespace: NS_CALL_INVITES_0,
  },
  {
    aliases: [{ path: "message.callReject", multiple: false }],
    element: "reject",
    fields: { id: attribute("id") },
    namespace: NS_CALL_INVITES_0,
  },
  {
    aliases: [{ path: "message.callRetract", multiple: false }],
    element: "retract",
    fields: { id: attribute("id") },
    namespace: NS_CALL_INVITES_0,
  },
  {
    aliases: [{ path: "message.callLeft", multiple: false }],
    element: "left",
    fields: { id: attribute("id") },
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
