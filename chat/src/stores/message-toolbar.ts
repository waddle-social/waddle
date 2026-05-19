import { atom } from "nanostores";

// Desktop reaction toolbars are mutually exclusive while an emoji picker is open.
export const $desktopToolbarOwnerId = atom<string | null>(null);
export const $desktopToolbarSuppressed = atom(false);
export const $desktopToolbarSuspensionEpoch = atom(0);

type ToolbarLifecycleTarget = {
  addEventListener: (
    type: string,
    listener: EventListener,
    options?: boolean | AddEventListenerOptions,
  ) => void;
  removeEventListener: (
    type: string,
    listener: EventListener,
    options?: boolean | EventListenerOptions,
  ) => void;
};

type ToolbarLifecycleDocument = ToolbarLifecycleTarget & {
  visibilityState?: DocumentVisibilityState;
};

export function clearDesktopToolbarOwner() {
  $desktopToolbarOwnerId.set(null);
}

function suppressDesktopToolbar() {
  clearDesktopToolbarOwner();
  $desktopToolbarSuppressed.set(true);
  $desktopToolbarSuspensionEpoch.set($desktopToolbarSuspensionEpoch.get() + 1);
}

function resumeDesktopToolbar() {
  $desktopToolbarSuppressed.set(false);
}

export function installMessageToolbarLifecycleSuppression(options: {
  windowTarget?: ToolbarLifecycleTarget | null;
  documentTarget?: ToolbarLifecycleDocument | null;
} = {}): () => void {
  const windowTarget = options.windowTarget ?? (typeof window === "undefined" ? null : window);
  const documentTarget = options.documentTarget ?? (typeof document === "undefined" ? null : document);
  let resumeListenersActive = false;

  const resumeFromInteraction: EventListener = () => {
    resumeDesktopToolbar();
    stopResumeListeners();
  };

  function startResumeListeners() {
    if (resumeListenersActive || !windowTarget) return;
    resumeListenersActive = true;
    windowTarget.addEventListener("pointerdown", resumeFromInteraction);
    windowTarget.addEventListener("pointermove", resumeFromInteraction);
    windowTarget.addEventListener("keydown", resumeFromInteraction, true);
  }

  function stopResumeListeners() {
    if (!resumeListenersActive || !windowTarget) return;
    resumeListenersActive = false;
    windowTarget.removeEventListener("pointerdown", resumeFromInteraction);
    windowTarget.removeEventListener("pointermove", resumeFromInteraction);
    windowTarget.removeEventListener("keydown", resumeFromInteraction, true);
  }

  const suspendForInactivePage: EventListener = () => {
    suppressDesktopToolbar();
    startResumeListeners();
  };

  const suspendWhenHidden: EventListener = () => {
    if (documentTarget?.visibilityState === "visible") return;
    suspendForInactivePage(new Event("visibilitychange"));
  };

  windowTarget?.addEventListener("blur", suspendForInactivePage);
  windowTarget?.addEventListener("pagehide", suspendForInactivePage);
  documentTarget?.addEventListener("visibilitychange", suspendWhenHidden);

  return () => {
    windowTarget?.removeEventListener("blur", suspendForInactivePage);
    windowTarget?.removeEventListener("pagehide", suspendForInactivePage);
    documentTarget?.removeEventListener("visibilitychange", suspendWhenHidden);
    stopResumeListeners();
    resumeDesktopToolbar();
  };
}
