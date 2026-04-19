/**
 * XEP-0369 (MIX-CORE), XEP-0405 (MIX-PAM), XEP-0407 (MIX-MISC).
 *
 * Wire shape:
 *   <iq type="set">
 *     <client-join xmlns="urn:xmpp:mix:pam:2" channel="general@mix.example.com">
 *       <join xmlns="urn:xmpp:mix:core:1">
 *         <subscribe node="urn:xmpp:mix:nodes:messages"/>
 *         <subscribe node="urn:xmpp:mix:nodes:participants"/>
 *         <nick>Alice</nick>
 *       </join>
 *     </client-join>
 *   </iq>
 */
import type { DefinitionOptions } from "stanza/jxt";
import { attribute, childText, splicePath } from "stanza/jxt";

export const NS_MIX_CORE_1 = "urn:xmpp:mix:core:1";
export const NS_MIX_PAM_2 = "urn:xmpp:mix:pam:2";
export const NS_MIX_MISC_0 = "urn:xmpp:mix:misc:0";

export const MIX_NODE_MESSAGES = "urn:xmpp:mix:nodes:messages";
export const MIX_NODE_PARTICIPANTS = "urn:xmpp:mix:nodes:participants";
export const MIX_NODE_INFO = "urn:xmpp:mix:nodes:info";

export interface WaddleMixSubscribe {
  node: string;
}

export interface WaddleMixCoreJoin {
  nick?: string;
  subscribes?: WaddleMixSubscribe[];
}

export interface WaddleMixClientJoin {
  channel: string;
  join: WaddleMixCoreJoin;
}

export interface WaddleMixClientLeave {
  channel: string;
}

export interface WaddleMixSetnick {
  nick: string;
}

const definitions: DefinitionOptions[] = [
  {
    aliases: [{ path: "iq.clientJoin", multiple: false }],
    element: "client-join",
    fields: {
      channel: attribute("channel"),
    },
    namespace: NS_MIX_PAM_2,
  },
  {
    aliases: [{ path: "iq.clientJoin.join", multiple: false }],
    element: "join",
    fields: {
      nick: childText(NS_MIX_CORE_1, "nick"),
      subscribes: splicePath(NS_MIX_CORE_1, "join", "subscribe", true),
    },
    namespace: NS_MIX_CORE_1,
  },
  {
    element: "subscribe",
    fields: {
      node: attribute("node"),
    },
    namespace: NS_MIX_CORE_1,
    path: "subscribe",
  },
  {
    aliases: [{ path: "iq.clientLeave", multiple: false }],
    element: "client-leave",
    fields: {
      channel: attribute("channel"),
    },
    namespace: NS_MIX_PAM_2,
  },
  {
    aliases: [{ path: "iq.clientLeave.leave", multiple: false }],
    element: "leave",
    fields: {},
    namespace: NS_MIX_CORE_1,
  },
  {
    aliases: [{ path: "iq.mixSetnick", multiple: false }],
    element: "setnick",
    fields: {
      nick: childText(NS_MIX_CORE_1, "nick"),
    },
    namespace: NS_MIX_CORE_1,
  },
];

export default definitions;
