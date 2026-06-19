import {
  computed,
  hasInjectionContext,
  inject,
  ref,
  watch,
  type ComputedRef,
  type InjectionKey,
  type Ref,
} from "vue";
import {
  copyCalendarFeedUrlToClipboard,
  isSameCalendarFeedRequestInput,
  nextCalendarFeedCopyViewState,
  type CalendarFeedCopyResult,
  type CalendarFeedCopyText,
  type CalendarFeedFetch,
  type CalendarFeedRequestInput,
} from "@/lib/calendar-feed-url";

type FeedCopyState = "idle" | "loading" | "copied" | "error";

export interface CalendarFeedCopySource {
  communityJid: () => string | null;
  serverBaseUrl: () => string;
  sessionId: () => string | null;
  fetch?: CalendarFeedFetch;
  copyText?: CalendarFeedCopyText;
  resetDelayMs?: number;
}

export interface CalendarFeedCopyController {
  canCopy: ComputedRef<boolean>;
  copy: () => Promise<void>;
  dispose: () => void;
  state: Ref<FeedCopyState>;
  statusLabel: ComputedRef<string>;
  url: Ref<string | null>;
}

export const calendarFeedCopyControllerKey: InjectionKey<CalendarFeedCopyController> = Symbol(
  "calendarFeedCopyController",
);

export function useCalendarFeedCopy(source: CalendarFeedCopySource): CalendarFeedCopyController {
  if (hasInjectionContext()) {
    const override = inject(calendarFeedCopyControllerKey, null);
    if (override) return override;
  }

  const state = ref<FeedCopyState>("idle");
  const url = ref<string | null>(null);
  let resetTimer: ReturnType<typeof setTimeout> | null = null;
  let attemptCounter = 0;

  const canCopy = computed(
    () => !!source.communityJid() && source.serverBaseUrl().trim().length > 0,
  );

  const statusLabel = computed(() => {
    if (state.value === "loading") return "Copying";
    if (state.value === "copied") return "Copied";
    if (state.value === "error") {
      return url.value ? "Couldn't copy" : "Couldn't get URL";
    }
    return "";
  });

  const stopContextWatch = watch(
    () => [source.communityJid(), source.serverBaseUrl(), source.sessionId()] as const,
    () => {
      attemptCounter += 1;
      applyTransition({ contextChanged: true });
    },
  );

  function currentRequestInput(): CalendarFeedRequestInput {
    return {
      communityJid: source.communityJid(),
      serverBaseUrl: source.serverBaseUrl(),
      sessionId: source.sessionId(),
    };
  }

  function scheduleReset(nextState: FeedCopyState) {
    if (resetTimer) clearTimeout(resetTimer);
    if (nextState === "copied" || nextState === "error") {
      resetTimer = setTimeout(() => {
        state.value = "idle";
        resetTimer = null;
      }, source.resetDelayMs ?? 2_000);
    } else {
      resetTimer = null;
    }
  }

  function applyTransition(input: {
    contextChanged?: boolean;
    startAttempt?: boolean;
    result?: CalendarFeedCopyResult;
  }) {
    const next = nextCalendarFeedCopyViewState({
      state: state.value,
      url: url.value,
      ...input,
    });
    state.value = next.state;
    url.value = next.url;
    scheduleReset(next.state);
  }

  async function copy() {
    if (!canCopy.value || state.value === "loading") return;
    const attempt = attemptCounter + 1;
    attemptCounter = attempt;
    const requestInput = currentRequestInput();
    applyTransition({ startAttempt: true });
    const result = await copyCalendarFeedUrlToClipboard(requestInput, {
      fetch: source.fetch,
      copyText: source.copyText,
    });
    if (
      attempt !== attemptCounter
      || !isSameCalendarFeedRequestInput(requestInput, currentRequestInput())
    ) {
      return;
    }
    applyTransition({ result });
  }

  function dispose() {
    attemptCounter += 1;
    stopContextWatch();
    if (resetTimer) clearTimeout(resetTimer);
    resetTimer = null;
  }

  return {
    canCopy,
    copy,
    dispose,
    state,
    statusLabel,
    url,
  };
}
