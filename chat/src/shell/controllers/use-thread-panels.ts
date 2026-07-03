import { type ComputedRef, type Ref, watch } from "vue";
import type { useChannelMessages } from "@/channels/messages";
import type { useChannelInbox } from "@/channels/inbox";
import type { useDirectMessageConversations } from "@/dms/conversations";
import type { useDirectMessages } from "@/dms/messages";
import type { useMessageThreads } from "@/channels/threads";
import type { useWaddleDirectory } from "@/waddles/directory";
import type { ChatShellState } from "@/shell/state";
import { resolveThreadEntryTarget } from "@/lib/threads-view-target";
import { nextThreadStack, sameThreadStack } from "@/lib/thread-stack";
import type { ExtensionRouteKey } from "@/shell/controllers/use-extension-routes";

export type ActiveRightPanel = "thread" | "pinned" | "extension";

interface ThreadPanelsDeps {
  ui: ChatShellState;
  waddles: ReturnType<typeof useWaddleDirectory>;
  messaging: ReturnType<typeof useChannelMessages>;
  dmMessaging: ReturnType<typeof useDirectMessages>;
  dmConversations: ReturnType<typeof useDirectMessageConversations>;
  channelUnread: ReturnType<typeof useChannelInbox>;
  threads: ReturnType<typeof useMessageThreads>;
  isActiveDirectDmSurface: () => boolean;
  activeChannelRoomJid: ComputedRef<string | null>;
  managedMucDomain: ComputedRef<string>;
  activeThreadStack: Ref<string[]>;
  activeThreadTargetMessageId: Ref<string | null>;
  activeThreadTargetRequestId: Ref<number>;
  activeRightPanel: Ref<ActiveRightPanel | null>;
  activeExtensionRouteKey: Ref<ExtensionRouteKey | null>;
  roomJidForChannelId: (channelId: string) => string | null;
  selectChannel: (channelId: string, options?: { roomJid?: string; surface?: "channels" | "dms" }) => Promise<void>;
  openDm: (peerJid: string) => Promise<void>;
  exitReactionMode: () => void;
  updateUrl: () => void;
}

/**
 * Thread panel state (the stack is a breadcrumb trail into nested
 * sub-threads; empty stack = panel closed — channels and DMs both use
 * XEP-0201 threads) plus the arbitration between the three mutually
 * exclusive right-rail panels (thread / pinned / extension).
 */
export function useThreadPanels(deps: ThreadPanelsDeps) {
  const {
    ui,
    waddles,
    messaging,
    dmMessaging,
    dmConversations,
    channelUnread,
    threads,
    isActiveDirectDmSurface,
    activeChannelRoomJid,
    managedMucDomain,
    activeThreadStack,
    activeThreadTargetMessageId,
    activeThreadTargetRequestId,
    activeRightPanel,
    activeExtensionRouteKey,
    roomJidForChannelId,
    selectChannel,
    openDm,
    exitReactionMode,
    updateUrl,
  } = deps;

  function getThreadLabel(threadId: string): string {
    const entry = threads.resolveEntry(threadId);
    const body = entry?.root?.body?.trim() ?? "";
    return body.length > 0 ? body.slice(0, 40) : threadId.slice(0, 8);
  }

  function isRightPanelAvailable(panel: ActiveRightPanel): boolean {
    if (ui.activePage.value !== "chat") return false;
    if (panel === "thread") return activeThreadStack.value.length > 0;
    if (panel === "pinned") {
      if (!ui.showPinnedPanel.value) return false;
      if (ui.sidebarMode.value === "dms") return !!dmConversations.activePeerJid.value;
      return ui.sidebarMode.value === "channels" && !!waddles.currentChannel.value && !!activeChannelRoomJid.value;
    }
    if (ui.sidebarMode.value !== "channels") return false;
    return !!activeExtensionRouteKey.value && !!waddles.currentChannel.value;
  }

  function bestAvailableRightPanel(exclude?: ActiveRightPanel): ActiveRightPanel | null {
    const candidates: ActiveRightPanel[] = ["thread", "pinned", "extension"];
    return candidates.find((candidate) => candidate !== exclude && isRightPanelAvailable(candidate)) ?? null;
  }

  function activateRightPanel(panel: ActiveRightPanel) {
    if (isRightPanelAvailable(panel)) activeRightPanel.value = panel;
  }

  function normalizeActiveRightPanel(exclude?: ActiveRightPanel) {
    if (activeRightPanel.value && activeRightPanel.value !== exclude && isRightPanelAvailable(activeRightPanel.value)) return;
    activeRightPanel.value = bestAvailableRightPanel(exclude);
  }

  function closeExtensionRoutePanel() {
    activeExtensionRouteKey.value = null;
    normalizeActiveRightPanel("extension");
  }

  function closePinnedPanel() {
    ui.showPinnedPanel.value = false;
    normalizeActiveRightPanel("pinned");
  }

  function openThread(threadId: string, targetMessageId?: string) {
    if (!threadId) return;
    activeRightPanel.value = "thread";
    activeThreadTargetMessageId.value = targetMessageId ?? null;
    if (targetMessageId) activeThreadTargetRequestId.value += 1;
    if (
      activeThreadStack.value.length > 0 &&
      activeThreadStack.value[activeThreadStack.value.length - 1] === threadId
    ) {
      if (targetMessageId) backfillActiveThread(threadId);
      return;
    }
    activeThreadStack.value = [threadId];
    backfillActiveThread(threadId);
  }

  // Backfill a thread's replies from the archive for the active surface. DMs
  // query the personal archive (`with=peer` + thread); channels query the
  // room archive. Without this a thread opened from the global Threads view
  // or a deep link shows only the replies that happen to be in the loaded
  // conversation window.
  function backfillActiveThread(threadId: string) {
    if (isActiveDirectDmSurface()) {
      void dmMessaging.backfillThread(threadId);
    } else if (ui.sidebarMode.value === "channels") {
      void messaging.backfillThread(threadId);
    } else if (waddles.currentChannel.value?.isGroupDm) {
      void messaging.backfillThread(threadId);
    }
  }

  async function onSelectThread(channelId: string, threadId: string) {
    // Navigate to the channel if not already there
    if (waddles.activeChannelId.value !== channelId) {
      await selectChannel(channelId);
    }
    // Mark thread as read
    const roomJid = roomJidForChannelId(channelId);
    if (roomJid) {
      channelUnread.markThreadRead(roomJid, threadId);
    }
    openThread(threadId);
  }

  // Global Threads view (`urn:waddle:threads:0`) rows carry a bare JID,
  // which may be a channel (MUC) room or a DM partner. Route each to its
  // own surface — channel selection vs DM open — then open the thread
  // panel. Without this, DM rows fell through `selectChannel(<localpart>)`
  // and tried to open a nonexistent channel (#917).
  async function onSelectThreadEntry(channelJid: string, threadId: string) {
    const target = resolveThreadEntryTarget(channelJid, {
      channels: waddles.channels.value,
      managedMucDomain: managedMucDomain.value,
    });
    if (!target) return;
    if (target.kind === "channel") {
      await onSelectThread(target.channelId, threadId);
      return;
    }
    await openDm(target.peerJid);
    openThread(threadId);
  }

  function pushThreadFromStack(baseStack: readonly string[], threadId: string) {
    if (!threadId) return;
    activeRightPanel.value = "thread";
    activeThreadTargetMessageId.value = null;
    const nextStack = nextThreadStack(baseStack, threadId);
    if (sameThreadStack(activeThreadStack.value, nextStack)) {
      return;
    }
    activeThreadStack.value = nextStack;
    backfillActiveThread(threadId);
  }

  function pushThread(threadId: string) {
    pushThreadFromStack(activeThreadStack.value, threadId);
  }

  function popThreadTo(index: number) {
    activeThreadTargetMessageId.value = null;
    if (index < 0) {
      activeThreadStack.value = [];
      normalizeActiveRightPanel("thread");
      return;
    }
    activeThreadStack.value = activeThreadStack.value.slice(0, index + 1);
    activeRightPanel.value = "thread";
  }

  function closeThreadPanel() {
    activeThreadTargetMessageId.value = null;
    activeThreadStack.value = [];
    normalizeActiveRightPanel("thread");
  }

  watch(
    [waddles.activeChannelId, ui.sidebarMode, () => dmConversations.activePeerJid.value],
    () => {
      // Channel / DM / mode changes close any open thread panel - the ids inside
      // the stack belong to the channel we just left.
      activeThreadTargetMessageId.value = null;
      activeThreadStack.value = [];
      // #414: pin panel state is per-room — clear on channel switch.
      ui.showPinnedPanel.value = false;
      activeExtensionRouteKey.value = null;
      activeRightPanel.value = null;
      exitReactionMode();
      updateUrl();
    },
  );

  watch(activeThreadStack, () => {
    normalizeActiveRightPanel();
    exitReactionMode();
    updateUrl();
  }, { deep: true });
  // #414: any toggle of the pin panel pushes the URL state.
  watch(() => ui.showPinnedPanel.value, () => {
    if (!ui.showPinnedPanel.value && activeRightPanel.value === "pinned") {
      normalizeActiveRightPanel("pinned");
    }
    updateUrl();
  });
  watch(activeExtensionRouteKey, () => {
    normalizeActiveRightPanel();
  }, { deep: true });
  watch(activeRightPanel, () => {
    updateUrl();
  });
  watch(() => ui.activePage.value, () => {
    normalizeActiveRightPanel();
    exitReactionMode();
    updateUrl();
  });

  return {
    getThreadLabel,
    activateRightPanel,
    normalizeActiveRightPanel,
    closeExtensionRoutePanel,
    closePinnedPanel,
    openThread,
    onSelectThread,
    onSelectThreadEntry,
    pushThreadFromStack,
    pushThread,
    popThreadTo,
    closeThreadPanel,
  };
}
