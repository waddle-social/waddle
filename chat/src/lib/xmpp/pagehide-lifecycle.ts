type PageLifecycleTarget = Pick<Window, "addEventListener" | "removeEventListener">;

export type PagehideXmppClient = {
  prepareForPageHide: () => void;
  resumeAfterPageShow: () => void;
};

/**
 * Own the browser pagehide handoff at the persistent XMPP lifecycle level.
 * Call UI may be idle or unmounted when a refresh happens; the live stream
 * still needs a best-effort acknowledgement request and synchronous snapshot.
 */
export function installXmppPagehideLifecycle(
  target: PageLifecycleTarget,
  currentClient: () => PagehideXmppClient | null,
  suspendCall: () => void,
): () => void {
  const handlePageHide = (event: PageTransitionEvent): void => {
    currentClient()?.prepareForPageHide();
    if (!event.persisted) suspendCall();
  };
  const handlePageShow = (event: PageTransitionEvent): void => {
    if (event.persisted) currentClient()?.resumeAfterPageShow();
  };

  target.addEventListener("pagehide", handlePageHide as EventListener);
  target.addEventListener("pageshow", handlePageShow as EventListener);
  return () => {
    target.removeEventListener("pagehide", handlePageHide as EventListener);
    target.removeEventListener("pageshow", handlePageShow as EventListener);
  };
}
