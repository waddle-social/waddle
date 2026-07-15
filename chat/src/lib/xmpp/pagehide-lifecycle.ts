type PagehideTarget = Pick<Window, "addEventListener" | "removeEventListener">;

export type PagehideXmppClient = {
  prepareForPageHide: () => void;
};

/**
 * Own the browser pagehide handoff at the persistent XMPP lifecycle level.
 * Call UI may be idle or unmounted when a refresh happens; the live stream
 * still needs a best-effort acknowledgement request and synchronous snapshot.
 */
export function installXmppPagehideLifecycle(
  target: PagehideTarget,
  currentClient: () => PagehideXmppClient | null,
  suspendCall: () => void,
): () => void {
  const handlePageHide = (event: PageTransitionEvent): void => {
    if (event.persisted) return;
    currentClient()?.prepareForPageHide();
    suspendCall();
  };

  target.addEventListener("pagehide", handlePageHide as EventListener);
  return () => target.removeEventListener("pagehide", handlePageHide as EventListener);
}
