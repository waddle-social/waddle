import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ExtensionAnnotationAction, TimelineMessage } from "@/lib/chat-ui";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { findMessageById } from "@/lib/message-ids";

// Outbound message-action handlers for the DM side: reactions (XEP-0444),
// retractions (XEP-0424), and extension-annotation actions. Mirrors the
// channel-side composable minus moderator retract (XEP-0425 is MUC-only).
//
// `applyReaction` is injected — the local optimistic-update path lives in
// the orchestrator today and moves into useDmLiveMerge in PR 4 of #351.

type UseDmMessageActionsDeps = {
  session: Ref<WaddleSession | null>;
  xmppClient: Ref<BrowserXmppClient | null>;
  activePeerJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  applyReaction: (messageId: string, nick: string, emojis: string[]) => void;
};

export function useDmMessageActions(deps: UseDmMessageActionsDeps) {
  const {
    session,
    xmppClient,
    activePeerJid,
    messages,
    actionError,
    clearActionError,
    normalizeError,
    applyReaction,
  } = deps;

  /**
   * XEP-0444 replace semantics for 1:1 chat. The sender device never
   * receives an XEP-0280 carbon of its own send, so the optimistic local
   * update is the only signal the author's reaction landed until a MAM
   * reload — rollback on send failure is essential.
   */
  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activePeerJid.value || !session.value) return;
    const msg = findMessageById(messages.value, messageId);
    const targetId = msg?.replyableId ?? msg?.id ?? messageId;
    const myNick = session.value.username;
    const currentReactions = msg?.reactions ?? {};
    const previousEmojis: string[] = [];
    for (const [e, nicks] of Object.entries(currentReactions)) {
      if (nicks.includes(myNick)) previousEmojis.push(e);
    }
    const myEmojis = new Set(previousEmojis);
    if (myEmojis.has(emoji)) myEmojis.delete(emoji);
    else myEmojis.add(emoji);

    const nextEmojis = [...myEmojis];
    applyReaction(targetId, myNick, nextEmojis);

    try {
      await xmppClient.value.sendDmReaction(activePeerJid.value, targetId, nextEmojis);
    } catch (e) {
      applyReaction(targetId, myNick, previousEmojis);
      actionError.value = normalizeError(e);
    }
  }

  /**
   * XEP-0424 retraction for 1:1. Prefer `replyableId` (origin-id per
   * XEP-0461 §3.2 in `dmMessageFromArchived`) over `TimelineMessage.id`
   * — the latter can fall back to the MAM archive id when origin-id and
   * the message's own `@id` are absent, and a MAM id is not a valid
   * XEP-0424 retraction target. The trailing `?? messageId` keeps the
   * caller's id usable when the message isn't in our timeline yet.
   */
  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activePeerJid.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.replyableId ?? target?.id ?? messageId;
    clearActionError();
    try {
      await xmppClient.value.sendDmRetraction(activePeerJid.value, targetId);
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  async function invokeExtensionAction(
    action: ExtensionAnnotationAction,
  ): Promise<ExtensionCommandResult> {
    const client = xmppClient.value;
    if (!client) {
      const error = new Error("XMPP session is not ready.");
      actionError.value = normalizeError(error);
      throw error;
    }
    if (!action.launch) {
      const error = new Error("This extension action is missing launch metadata.");
      actionError.value = normalizeError(error);
      throw error;
    }
    clearActionError();
    try {
      return await client.invokeExtensionLaunch(action.launch);
    } catch (e) {
      actionError.value = normalizeError(e);
      throw e;
    }
  }

  return {
    toggleReaction,
    retractMessage,
    invokeExtensionAction,
  };
}
