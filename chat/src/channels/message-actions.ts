import type { Ref } from "vue";
import type { WaddleSession } from "@/lib/server-auth";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ExtensionAnnotationAction, TimelineMessage } from "@/lib/chat-ui";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { findMessageById } from "@/lib/message-ids";

// Derive an XEP-0201 thread reference from a timeline message, if it belongs
// to one. Returns undefined for top-level (non-threaded) messages so callers
// pass plain channel-level stanzas in that case.
function threadRefFromMessage(
  message: TimelineMessage | undefined,
): { id: string; parent?: string } | undefined {
  const id = message?.threadId;
  if (!id) return undefined;
  const parent = message.parentThreadId;
  return parent ? { id, parent } : { id };
}

// Outbound message-action handlers for the channel side: reactions
// (XEP-0444), retractions (XEP-0424), moderator retractions (XEP-0425), and
// extension-annotation actions. Each action targets an already-rendered
// timeline message via its `replyableId` (the room's XEP-0359 stanza-id) or
// `reactionTargetId`, so the composable doesn't construct stanzas — it
// reads the right id off `messages` and forwards to the wasm client.
//
// `applyReaction` is injected as a dep because the local optimistic-update
// path is owned by the live-merge composable (PR 4 of #351). Until that
// extraction lands, the orchestrator passes its own `applyReaction` inline
// function through here.

type UseChannelMessageActionsDeps = {
  session: Ref<WaddleSession | null>;
  xmppClient: Ref<BrowserXmppClient | null>;
  activeSpaceId: Ref<string | null>;
  activeChannelId: Ref<string | null>;
  currentRoomJid: Ref<string | null>;
  messages: Ref<TimelineMessage[]>;
  actionError: Ref<string>;
  clearActionError: () => void;
  normalizeError: (e: unknown) => string;
  // Optimistic local-reaction update. PR 4 (live-merge) owns the canonical
  // implementation; for now the orchestrator passes its inline version.
  applyReaction: (messageId: string, nick: string, emojis: string[], senderId?: string) => void;
};

export function useChannelMessageActions(deps: UseChannelMessageActionsDeps) {
  const {
    session,
    xmppClient,
    activeSpaceId,
    activeChannelId,
    currentRoomJid,
    messages,
    actionError,
    clearActionError,
    normalizeError,
    applyReaction,
  } = deps;

  /**
   * XEP-0444 §3: toggle the local user's reaction. Reads the current
   * per-author set, flips this emoji, and sends the FULL updated set —
   * receivers replace the prior set per replace-semantics. An optimistic
   * local update applies before the wire round-trip and rolls back if the
   * server rejects.
   */
  async function toggleReaction(messageId: string, emoji: string) {
    if (!xmppClient.value || !activeChannelId.value || !session.value) return;

    const msg = findMessageById(messages.value, messageId);
    const targetId = msg?.reactionTargetId;
    if (!targetId) return;
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
    const senderId = currentRoomJid.value
      ? `${currentRoomJid.value}/${myNick}`
      : myNick;
    applyReaction(targetId, myNick, nextEmojis, senderId);

    try {
      // XEP-0201 §3 + XEP-0444: reactions targeting a threaded message echo
      // that message's <thread/>. Receivers without thread-aware UIs ignore it;
      // thread-aware peers attribute the reaction to the right conversation.
      const thread = threadRefFromMessage(msg);
      await xmppClient.value.sendReaction(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        targetId,
        nextEmojis,
        thread,
      );
    } catch (e) {
      // Roll back the optimistic update so the chip stops claiming a
      // reaction the server never accepted (no echo will arrive to
      // reconcile it).
      applyReaction(targetId, myNick, previousEmojis, senderId);
      actionError.value = normalizeError(e);
    }
  }

  /**
   * XEP-0424: retract the message identified by its room-assigned stanza-id
   * (`replyableId`). Refuses to send when the id is missing — origin-id
   * isn't trustworthy enough to retract by per XEP-0359 §5.
   */
  async function retractMessage(messageId: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.replyableId;

    clearActionError();
    if (!targetId) {
      actionError.value =
        "This message can't be retracted (no room stanza-id). Try reloading the channel.";
      return;
    }

    try {
      // XEP-0201 §3 + XEP-0424: tombstone targeting a threaded message echoes
      // that message's <thread/> so the retraction routes into the same
      // thread on receiving clients instead of the parent channel.
      const thread = threadRefFromMessage(target);
      await xmppClient.value.sendRetraction(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        targetId,
        thread,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  /**
   * XEP-0425: moderator retract (MUC only). Same id-trust requirement as
   * `retractMessage` — uses the room stanza-id, not origin-id.
   */
  async function moderateMessage(messageId: string, reason?: string) {
    if (!xmppClient.value || !activeChannelId.value) return;
    const target = findMessageById(messages.value, messageId);
    const targetId = target?.replyableId;

    clearActionError();
    if (!targetId) {
      actionError.value =
        "This message can't be moderated (no room stanza-id). Try reloading the channel.";
      return;
    }

    try {
      await xmppClient.value.sendModeration(
        activeSpaceId.value ?? "",
        activeChannelId.value,
        targetId,
        reason,
      );
    } catch (e) {
      actionError.value = normalizeError(e);
    }
  }

  /**
   * Outbound extension-annotation action — Waddle-specific. The wasm client
   * resolves the launch URL into the extension's command flow.
   */
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
    moderateMessage,
    invokeExtensionAction,
  };
}
