import { Effect, Exit, Scope } from "effect";

type PageLifecycleTarget = Pick<Window, "addEventListener" | "removeEventListener">;

export type PagehideXmppClient = {
  prepareForPageHide: () => void;
  resumeAfterPageShow: () => void;
};

type XmppPageLifecycleOperation =
  | "prepare-xmpp"
  | "resume-xmpp"
  | "suspend-call";

export type XmppPageLifecycleFailure = {
  readonly operation: XmppPageLifecycleOperation;
  readonly cause: unknown;
};

export class PageLifecycleInstallError extends Error {
  readonly _tag = "PageLifecycleInstallError";

  constructor(readonly cause: unknown) {
    super("failed to install XMPP page lifecycle listeners");
    this.name = "PageLifecycleInstallError";
  }
}

type InstalledPageLifecycle = {
  readonly pagehide: EventListener;
  readonly pageshow: EventListener;
};

function reportLifecycleFailure(
  reportFailure: (failure: XmppPageLifecycleFailure) => void,
  failure: XmppPageLifecycleFailure,
): void {
  try {
    reportFailure(failure);
  } catch {
    // Observability must never interrupt the synchronous browser lifecycle.
  }
}

/**
 * Effect-scoped listener ownership. Acquisition is failure-atomic: if the
 * second listener cannot be installed, the first is removed before the
 * typed acquisition failure escapes.
 */
export function acquireXmppPagehideLifecycle(
  target: PageLifecycleTarget,
  currentClient: () => PagehideXmppClient | null,
  suspendCall: () => void,
  reportFailure: (failure: XmppPageLifecycleFailure) => void = () => undefined,
): Effect.Effect<void, PageLifecycleInstallError, Scope.Scope> {
  const handlePageHide = (event: PageTransitionEvent): void => {
    try {
      currentClient()?.prepareForPageHide();
    } catch (cause) {
      reportLifecycleFailure(reportFailure, { operation: "prepare-xmpp", cause });
    } finally {
      if (!event.persisted) {
        try {
          suspendCall();
        } catch (cause) {
          reportLifecycleFailure(reportFailure, { operation: "suspend-call", cause });
        }
      }
    }
  };
  const handlePageShow = (event: PageTransitionEvent): void => {
    if (!event.persisted) return;
    try {
      currentClient()?.resumeAfterPageShow();
    } catch (cause) {
      reportLifecycleFailure(reportFailure, { operation: "resume-xmpp", cause });
    }
  };
  const pagehide = handlePageHide as EventListener;
  const pageshow = handlePageShow as EventListener;

  const acquire = Effect.try({
    try: (): InstalledPageLifecycle => {
      target.addEventListener("pagehide", pagehide);
      try {
        target.addEventListener("pageshow", pageshow);
      } catch (cause) {
        target.removeEventListener("pagehide", pagehide);
        throw cause;
      }
      return { pagehide, pageshow };
    },
    catch: (cause) => new PageLifecycleInstallError(cause),
  });

  return Effect.acquireRelease(
    acquire,
    (installed) =>
      Effect.sync(() => {
        try {
          target.removeEventListener("pageshow", installed.pageshow);
        } finally {
          target.removeEventListener("pagehide", installed.pagehide);
        }
      }),
  ).pipe(Effect.asVoid);
}

/**
 * Own the browser pagehide handoff at the persistent XMPP lifecycle level.
 * Call UI may be idle or unmounted when a refresh happens; the live stream
 * still needs a best-effort acknowledgement request and synchronous snapshot.
 */
export function installXmppPagehideLifecycle(
  target: PageLifecycleTarget,
  currentClient: () => PagehideXmppClient | null,
  suspendCall: () => void,
  reportFailure: (failure: XmppPageLifecycleFailure) => void = () => undefined,
): () => void {
  const scope = Effect.runSync(Scope.make());
  try {
    Effect.runSync(
      Scope.extend(
        acquireXmppPagehideLifecycle(target, currentClient, suspendCall, reportFailure),
        scope,
      ),
    );
  } catch (cause) {
    Effect.runSync(Scope.close(scope, Exit.fail(cause)));
    throw cause;
  }

  let closed = false;
  return () => {
    if (closed) return;
    closed = true;
    Effect.runSync(Scope.close(scope, Exit.succeed(undefined)));
  };
}
