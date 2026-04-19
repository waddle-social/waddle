/**
 * MIX (XEP-0369 / XEP-0405) outbound operations.
 *
 * Mirrors the standalone-send-function shape used by `messaging.ts` and
 * `dm-messaging.ts`. The transport is `Agent` from stanza.js; the MIX
 * stanza definitions live in `extensions/mix.ts`.
 */
import type { Agent } from "stanza";
import {
  MIX_NODE_MESSAGES,
  MIX_NODE_PARTICIPANTS,
  type WaddleMixSubscribe,
} from "./extensions/mix";

const DEFAULT_NODES: WaddleMixSubscribe[] = [
  { node: MIX_NODE_MESSAGES },
  { node: MIX_NODE_PARTICIPANTS },
];

/**
 * Subscribe to a MIX channel via MIX-PAM `client-join`. The caller's
 * own server proxies the subscription to the remote channel host.
 */
export async function joinMixChannel(
  xmpp: Agent,
  channelJid: string,
  nick: string,
  nodes: WaddleMixSubscribe[] = DEFAULT_NODES,
): Promise<unknown> {
  return xmpp.sendIQ({
    type: "set",
    clientJoin: {
      channel: channelJid,
      join: {
        nick,
        subscribes: nodes,
      },
    },
  } as Parameters<Agent["sendIQ"]>[0]);
}

/**
 * Unsubscribe from a MIX channel via MIX-PAM `client-leave`.
 */
export async function leaveMixChannel(
  xmpp: Agent,
  channelJid: string,
): Promise<unknown> {
  return xmpp.sendIQ({
    type: "set",
    clientLeave: {
      channel: channelJid,
      leave: {},
    },
  } as Parameters<Agent["sendIQ"]>[0]);
}

/**
 * Change the caller's nick on a MIX channel via `<setnick>`.
 */
export async function setMixChannelNick(
  xmpp: Agent,
  channelJid: string,
  nick: string,
): Promise<unknown> {
  return xmpp.sendIQ({
    type: "set",
    to: channelJid,
    mixSetnick: { nick },
  } as Parameters<Agent["sendIQ"]>[0]);
}

/**
 * Publish a message to a MIX channel. Wire form mirrors MUC
 * `type='groupchat'` per XEP-0369 §7.1; the routing distinguisher is
 * the `mix.<domain>` subdomain on the destination JID.
 */
export function sendMixMessage(
  xmpp: Agent,
  channelJid: string,
  body: string,
  thread?: string,
  msgId?: string,
): string {
  const id = msgId ?? cryptoRandomId();
  const message: Record<string, unknown> = {
    id,
    to: channelJid,
    type: "groupchat",
    body,
  };
  if (thread) {
    message.thread = { id: thread };
  }
  xmpp.sendMessage(message as Parameters<Agent["sendMessage"]>[0]);
  return id;
}

function cryptoRandomId(): string {
  const arr = new Uint8Array(8);
  crypto.getRandomValues(arr);
  return Array.from(arr)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
