import { Effect, Exit, Scope } from "effect";

type PageLifecycleTarget = Pick<Window, "addEventListener" | "removeEventListener">;

export type PagehideXmppClient = {
  prepareForPageHide: () => void;
  resumeAfterPageShow: () => void;
};

type XmppPageLifecycleOperation = "prepare-xmpp" | "resume-xmpp" | "suspend-call";

export type XmppPageLifecycleFailure = {
  readonly operation: XmppPageLifecycleOperation;
};

class PageLifecycleInstallError extends Error {
  readonly _tag = "PageLifecycleInstallError";

  constructor() {
    super("failed to install XMPP page lifecycle listeners");
    this.name = "PageLifecycleInstallError";
  }
}

type InstalledPageLifecycle = {
  readonly pagehide: EventListener;
  readonly pageshow: EventListener;
};

function safelyReport(
  reportFailure: (failure: XmppPageLifecycleFailure) => void,
  failure: XmppPageLifecycleFailure,
): void {
  try {
    reportFailure(failure);
  } catch {
    // Lifecycle teardown must not be interrupted by observability.
  }
}

function acquireXmppPagehideLifecycle(
  target: PageLifecycleTarget,
  currentClient: () => PagehideXmppClient | null,
  suspendCall: () => void,
  reportFailure: (failure: XmppPageLifecycleFailure) => void = () => undefined,
): Effect.Effect<void, PageLifecycleInstallError, Scope.Scope> {
  const pagehide = ((event: PageTransitionEvent): void => {
    try {
      currentClient()?.prepareForPageHide();
    } catch {
      safelyReport(reportFailure, { operation: "prepare-xmpp" });
    } finally {
      if (!event.persisted) {
        try {
          suspendCall();
        } catch {
          safelyReport(reportFailure, { operation: "suspend-call" });
        }
      }
    }
  }) as EventListener;
  const pageshow = ((event: PageTransitionEvent): void => {
    if (!event.persisted) return;
    try {
      currentClient()?.resumeAfterPageShow();
    } catch {
      safelyReport(reportFailure, { operation: "resume-xmpp" });
    }
  }) as EventListener;

  return Effect.acquireRelease(
    Effect.try({
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
      catch: () => new PageLifecycleInstallError(),
    }),
    (installed) => Effect.sync(() => {
      try {
        target.removeEventListener("pageshow", installed.pageshow);
      } finally {
        target.removeEventListener("pagehide", installed.pagehide);
      }
    }),
  ).pipe(Effect.asVoid);
}

export function installXmppPagehideLifecycle(
  target: PageLifecycleTarget,
  currentClient: () => PagehideXmppClient | null,
  suspendCall: () => void,
  reportFailure: (failure: XmppPageLifecycleFailure) => void = () => undefined,
): () => void {
  const scope = Effect.runSync(Scope.make());
  try {
    Effect.runSync(Scope.extend(acquireXmppPagehideLifecycle(target, currentClient, suspendCall, reportFailure), scope));
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
