import { computed, type ComputedRef, type Ref, ref, watch } from "vue";
import type { useMessageThreads } from "@/channels/threads";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import type { TimelineMessage } from "@/lib/chat-ui";
import { orderTimelineForScrollDirection, type ScrollDirectionMode } from "@/lib/scroll-direction";
import {
  moveReactionSelection,
  preserveReactionSelection,
  quickReactionForKey,
  selectInitialReactionMessage,
  type ReactionModeScope,
} from "@/lib/reaction-mode";
import {
  consumeKeystrokEvent,
  type KeystrokHandle,
  REACTION_MODE_KEYSTROK_SCOPE,
} from "@/shell/controllers/use-chat-keyboard";
import type { ActiveRightPanel } from "@/shell/controllers/use-thread-panels";

type ReactionModeTarget = "main" | "thread";

interface ReactionModeDeps {
  ui: ChatShellState;
  scrollDirectionMode: Ref<ScrollDirectionMode>;
  waddles: ReturnType<typeof useWaddleDirectory>;
  threads: ReturnType<typeof useMessageThreads>;
  activeMessages: ComputedRef<TimelineMessage[]>;
  activeThreadStack: Ref<string[]>;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  activeDmPeer: ComputedRef<{ peerJid: string } | null>;
  reactMessage: (messageId: string, emoji: string) => void;
  anyModalOpen: () => boolean;
  keystrok: KeystrokHandle;
}

/**
 * Keyboard-driven reaction mode: "+" enters a selection mode over the
 * feed or the open thread, arrows move the selection, 1–5 apply a quick
 * reaction, and Escape/typing yields back to normal input.
 */
export function useReactionMode(deps: ReactionModeDeps) {
  const {
    ui,
    scrollDirectionMode,
    waddles,
    threads,
    activeMessages,
    activeThreadStack,
    activeRightPanel,
    activeDmPeer,
    reactMessage,
    anyModalOpen,
    keystrok,
  } = deps;

  const reactionModeTarget = ref<ReactionModeTarget | null>(null);
  const reactionModeSelectedMessageId = ref<string | null>(null);

  const orderedMainReactionMessages = computed(() =>
    orderTimelineForScrollDirection(
      reactionModeMessageCandidates(activeMessages.value.filter((message) => !message.threadId || message.id === message.threadId)),
      scrollDirectionMode.value,
    ),
  );

  const activeThreadReactionMessages = computed<TimelineMessage[]>(() => {
    const threadId = activeThreadStack.value[activeThreadStack.value.length - 1];
    if (!threadId) return [];
    const entry = threads.resolveEntry(threadId);
    if (!entry) return [];
    const orderedChildren = orderTimelineForScrollDirection(reactionModeMessageCandidates(entry.directChildren), scrollDirectionMode.value);
    return entry.root ? reactionModeMessageCandidates([entry.root, ...orderedChildren]) : orderedChildren;
  });

  const reactionModeMessages = computed(() =>
    reactionModeTarget.value === "thread"
      ? activeThreadReactionMessages.value
      : orderedMainReactionMessages.value,
  );

  const reactionModeScope = computed<ReactionModeScope>(() =>
    reactionModeTarget.value === "thread" ? "thread" : "feed",
  );

  const reactionModeState = computed(() =>
    reactionModeTarget.value
      ? {
          selectedMessageId: reactionModeSelectedMessageId.value,
        }
      : null,
  );

  function activeReactionTarget(): ReactionModeTarget | null {
    if (ui.activePage.value !== "chat") return null;
    if (ui.sidebarMode.value === "channels" && activeRightPanel.value === "thread" && activeThreadStack.value.length > 0) return "thread";
    if (ui.sidebarMode.value === "dms" && activeDmPeer.value) return "main";
    if (ui.sidebarMode.value === "channels" && waddles.currentChannel.value) return "main";
    return null;
  }

  function reactionModeMessageCandidates(messages: readonly TimelineMessage[]): TimelineMessage[] {
    return messages.map((message) => ({
      ...message,
      canReact: ui.sidebarMode.value === "dms" || !!message.reactionTargetId,
    }));
  }

  function exitReactionMode() {
    reactionModeTarget.value = null;
    reactionModeSelectedMessageId.value = null;
    keystrok.current?.scope(REACTION_MODE_KEYSTROK_SCOPE).deactivate();
  }

  function startReactionMode(target = activeReactionTarget()): boolean {
    if (!target) return false;
    const messages = target === "thread" ? activeThreadReactionMessages.value : orderedMainReactionMessages.value;
    const scope = target === "thread" ? "thread" : "feed";
    const selected = selectInitialReactionMessage(messages, scope);
    if (!selected) return false;
    reactionModeTarget.value = target;
    reactionModeSelectedMessageId.value = selected;
    keystrok.current?.scope(REACTION_MODE_KEYSTROK_SCOPE).override();
    return true;
  }

  function reactToSelectedMessage(emoji: string) {
    const messageId = reactionModeSelectedMessageId.value;
    if (!messageId) return;
    reactMessage(messageId, emoji);
    exitReactionMode();
  }

  function isEditableKeyTarget(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    if (target.closest("[contenteditable='true']")) return true;
    return !!target.closest("input, textarea, select");
  }

  function isComposerEditorTarget(target: EventTarget | null): boolean {
    return target instanceof HTMLElement && !!target.closest(".chat-composer .ProseMirror");
  }

  function isComposerEditorEmpty(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    const editor = target.closest(".chat-composer .ProseMirror");
    if (!(editor instanceof HTMLElement)) return false;
    return (editor.textContent ?? "").length === 0;
  }

  function canStartReactionModeFromEvent(event: KeyboardEvent): boolean {
    if (anyModalOpen()) return false;
    if (!isEditableKeyTarget(event.target)) return true;
    if (!isComposerEditorTarget(event.target)) return false;
    return isComposerEditorEmpty(event.target);
  }

  function handleStartReactionModeShortcut(event: KeyboardEvent) {
    if (!canStartReactionModeFromEvent(event)) return;
    if (startReactionMode()) {
      consumeKeystrokEvent(event);
    }
  }

  function handleLiteralPlusKeyDown(event: KeyboardEvent) {
    if (event.key !== "+" || event.ctrlKey || event.metaKey || event.altKey) return;
    if (reactionModeTarget.value) {
      if (!ensureReactionModeOwnsEvent(event)) return;
      consumeKeystrokEvent(event);
      return;
    }
    handleStartReactionModeShortcut(event);
  }

  function shouldYieldReactionModeForEvent(event: KeyboardEvent): boolean {
    if (anyModalOpen()) return true;
    return isEditableKeyTarget(event.target);
  }

  function ensureReactionModeOwnsEvent(event: KeyboardEvent): boolean {
    if (!shouldYieldReactionModeForEvent(event)) return true;
    exitReactionMode();
    return false;
  }

  function handleReactionModeMove(event: KeyboardEvent, direction: "previous" | "next") {
    if (!ensureReactionModeOwnsEvent(event)) return;
    reactionModeSelectedMessageId.value = moveReactionSelection(
      reactionModeSelectedMessageId.value,
      reactionModeMessages.value,
      reactionModeScope.value,
      direction,
    );
    consumeKeystrokEvent(event);
  }

  function handleReactionModeQuickReaction(event: KeyboardEvent) {
    if (!ensureReactionModeOwnsEvent(event)) return;
    const emoji = quickReactionForKey(event.key);
    if (emoji) reactToSelectedMessage(emoji);
    consumeKeystrokEvent(event);
  }

  function handleReactionModeEscape(event: KeyboardEvent) {
    const shouldYield = shouldYieldReactionModeForEvent(event);
    exitReactionMode();
    if (!shouldYield) consumeKeystrokEvent(event);
  }

  watch(
    [reactionModeMessages, reactionModeScope],
    () => {
      if (!reactionModeTarget.value) return;
      const selected = preserveReactionSelection(
        reactionModeSelectedMessageId.value,
        reactionModeMessages.value,
        reactionModeScope.value,
      );
      if (!selected) {
        exitReactionMode();
        return;
      }
      reactionModeSelectedMessageId.value = selected;
    },
    { flush: "post" },
  );

  return {
    reactionModeTarget,
    reactionModeState,
    exitReactionMode,
    handleLiteralPlusKeyDown,
    handleReactionModeEscape,
    handleReactionModeMove,
    handleReactionModeQuickReaction,
  };
}
