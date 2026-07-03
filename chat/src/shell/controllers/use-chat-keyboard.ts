import { onMounted, onUnmounted, type Ref } from "vue";
import { createKeystrok, type Keystrok } from "keystrok";
import type { ChatShellState } from "@/shell/state";
import type { ActiveRightPanel } from "@/shell/controllers/use-thread-panels";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";

const CHAT_KEYSTROK_SCOPE = "chat";
export const REACTION_MODE_KEYSTROK_SCOPE = "chat-reaction-mode";

/** Shared handle to the lazily created keystrok instance: the keyboard
 * composable owns creation/destruction, reaction mode toggles its scope. */
export interface KeystrokHandle {
  current: Keystrok | null;
}

export function anyModalOpen(ui: ChatShellState): boolean {
  const domModalOpen = typeof document !== "undefined" && !!document.querySelector("[aria-modal='true']");
  return ui.showCreateChannel.value ||
    ui.showEditChannel.value ||
    ui.showWaddleSettings.value ||
    ui.showMembers.value ||
    ui.confirmDeleteWaddle.value ||
    ui.confirmDeleteChannel.value ||
    ui.showNewDm.value ||
    ui.confirmRemoveMember.value !== null ||
    ui.showMobileNav.value ||
    ui.showMobileDetails.value ||
    domModalOpen;
}

export function consumeKeystrokEvent(event: KeyboardEvent) {
  event.preventDefault();
  event.stopPropagation();
}

interface ChatKeyboardDeps {
  ui: ChatShellState;
  keystrok: KeystrokHandle;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  activeThreadStack: Ref<string[]>;
  closeExtensionRoutePanel: () => void;
  closePinnedPanel: () => void;
  normalizeActiveRightPanel: () => void;
  reactionMode: {
    handleLiteralPlusKeyDown: (event: KeyboardEvent) => void;
    handleReactionModeEscape: (event: KeyboardEvent) => void;
    handleReactionModeMove: (event: KeyboardEvent, direction: "previous" | "next") => void;
    handleReactionModeQuickReaction: (event: KeyboardEvent) => void;
  };
}

/**
 * Chat-page keyboard shortcuts: the Escape ladder over the right-rail
 * panels and the keystrok scopes that host reaction mode's bindings.
 */
export function useChatKeyboard(deps: ChatKeyboardDeps) {
  const {
    ui,
    keystrok,
    activeRightPanel,
    activeExtensionRouteKey,
    activeThreadStack,
    closeExtensionRoutePanel,
    closePinnedPanel,
    normalizeActiveRightPanel,
    reactionMode,
  } = deps;

  function handleChatEscape(event: KeyboardEvent) {
    // Don't intercept Escape when any dialog/drawer is open so they can close first.
    if (anyModalOpen(ui)) return;
    if (activeRightPanel.value === "extension" && activeExtensionRouteKey.value) {
      closeExtensionRoutePanel();
      consumeKeystrokEvent(event);
      return;
    }
    if (activeRightPanel.value === "pinned" && ui.showPinnedPanel.value) {
      closePinnedPanel();
      consumeKeystrokEvent(event);
      return;
    }
    if (activeThreadStack.value.length === 0) return;
    activeThreadStack.value = activeThreadStack.value.slice(0, -1);
    normalizeActiveRightPanel();
    consumeKeystrokEvent(event);
  }

  function bindChatKeystrokShortcuts() {
    const chatKeystrok = createKeystrok();
    keystrok.current = chatKeystrok;
    chatKeystrok
      .bind("escape", handleChatEscape, { scope: CHAT_KEYSTROK_SCOPE })
      .bind("escape", reactionMode.handleReactionModeEscape, { scope: REACTION_MODE_KEYSTROK_SCOPE })
      .bind("up", (event) => reactionMode.handleReactionModeMove(event, "previous"), { scope: REACTION_MODE_KEYSTROK_SCOPE })
      .bind("down", (event) => reactionMode.handleReactionModeMove(event, "next"), { scope: REACTION_MODE_KEYSTROK_SCOPE })
      .bind("backspace", reactionMode.handleReactionModeEscape, { scope: REACTION_MODE_KEYSTROK_SCOPE })
      .bind("delete", reactionMode.handleReactionModeEscape, { scope: REACTION_MODE_KEYSTROK_SCOPE });

    for (const key of ["1", "2", "3", "4", "5"]) {
      chatKeystrok.bind(key, reactionMode.handleReactionModeQuickReaction, { scope: REACTION_MODE_KEYSTROK_SCOPE });
    }

    chatKeystrok.scope(CHAT_KEYSTROK_SCOPE).activate();
  }

  onMounted(() => {
    // keystrok uses "+" as its shortcut separator, so the literal plus key is
    // handled directly while the rest of reaction mode remains scoped there.
    window.addEventListener("keydown", reactionMode.handleLiteralPlusKeyDown, true);
    bindChatKeystrokShortcuts();
  });

  onUnmounted(() => {
    window.removeEventListener("keydown", reactionMode.handleLiteralPlusKeyDown, true);
    keystrok.current?.destroy();
    keystrok.current = null;
  });
}
