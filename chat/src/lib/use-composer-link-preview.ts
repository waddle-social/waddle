import { computed, onScopeDispose, ref, watch, type Ref } from "vue";
import {
  composerLinkPreviewUrl,
  linkPreviewStateFromLookup,
  sendPayloadFromState,
  type ComposerLinkPreviewLookup,
  type ComposerLinkPreviewSendPayload,
  type ComposerLinkPreviewState,
} from "@/lib/link-preview-composer";

// Bounded wait at send time for a lookup that is still in flight. The lookup
// itself may run longer in the background (8 s); send must stay responsive
// and fail open without a preview rather than block on a slow origin.
export const SEND_LOOKUP_GRACE_MS = 2_250;

// Quiet period after the draft's link changes before the server lookup runs,
// so editing a link character by character issues one lookup, not one per key.
export const LOOKUP_DEBOUNCE_MS = 400;

export function useComposerLinkPreview(
  draft: Ref<string>,
  lookup: Ref<ComposerLinkPreviewLookup | null | undefined>,
  scopeKey: Ref<string | null | undefined>,
  debounceMs = LOOKUP_DEBOUNCE_MS,
) {
  const state = ref<ComposerLinkPreviewState>({ kind: "idle" });
  let lookupEpoch = 0;
  let activeKey: string | null = null;
  let activeLookup: ComposerLinkPreviewLookup | null = null;
  let activeLookupSettled: Promise<ComposerLinkPreviewState> | null = null;
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  let pendingStart: (() => void) | null = null;

  function cancelPendingLookup() {
    if (debounceTimer !== undefined) clearTimeout(debounceTimer);
    debounceTimer = undefined;
    pendingStart = null;
  }
  onScopeDispose(cancelPendingLookup);

  const showCard = computed(() => state.value.kind !== "idle");
  const host = computed(() => {
    const url = "url" in state.value ? state.value.url : "";
    if (!url) return "";
    try {
      return new URL(url).hostname.replace(/^www\./, "");
    } catch {
      return url;
    }
  });
  const title = computed(() => {
    if (state.value.kind === "ready") return state.value.payload.preview.title ?? host.value;
    if (state.value.kind === "loading") return "Loading preview";
    if (state.value.kind === "unsupported") return "Preview unavailable";
    if (state.value.kind === "failed") return "Preview failed";
    if (state.value.kind === "dismissed") return "Preview removed";
    return "";
  });
  const description = computed(() => {
    if (state.value.kind === "ready") return state.value.payload.preview.description ?? state.value.payload.preview.originalUrl;
    if (state.value.kind === "loading") return host.value;
    if (state.value.kind === "unsupported") return host.value;
    if (state.value.kind === "failed") return host.value;
    if (state.value.kind === "dismissed") return host.value;
    return "";
  });

  function dismiss() {
    if (!("url" in state.value)) return;
    lookupEpoch++;
    cancelPendingLookup();
    activeKey = stateKey(scopeKey.value, state.value.url);
    activeLookup = null;
    activeLookupSettled = null;
    state.value = { kind: "dismissed", url: state.value.url };
  }

  async function sendPayloadFor(body: string): Promise<ComposerLinkPreviewSendPayload | undefined> {
    const url = composerLinkPreviewUrl(body);
    const key = stateKey(scopeKey.value, url);
    if (!key) return undefined;

    if (key !== activeKey) return undefined;
    // Sending ends the typing pause: start a still-debounced lookup now.
    pendingStart?.();

    const pending = state.value.kind === "loading" ? activeLookupSettled : null;
    if (pending) {
      const settledState = await settleWithGrace(pending);
      return settledState ? sendPayloadFromState(settledState) : undefined;
    }

    return sendPayloadFromState(state.value);
  }

  watch(
    () => [draft.value, lookup.value, scopeKey.value] as const,
    ([body, lookupFn, scope]) => {
      const url = composerLinkPreviewUrl(body);
      const key = stateKey(scope, url);
      if (!url || !lookupFn || !key) {
        lookupEpoch++;
        cancelPendingLookup();
        activeKey = null;
        activeLookup = null;
        activeLookupSettled = null;
        state.value = { kind: "idle" };
        return;
      }
      if (
        key === activeKey &&
        state.value.kind !== "idle" &&
        (state.value.kind === "dismissed" || activeLookup === lookupFn)
      ) {
        return;
      }

      const epoch = ++lookupEpoch;
      cancelPendingLookup();
      activeKey = key;
      activeLookup = lookupFn;
      state.value = { kind: "loading", url };
      if (debounceMs <= 0) {
        startLookup(epoch, url, body, lookupFn);
        return;
      }
      pendingStart = () => startLookup(epoch, url, body, lookupFn);
      debounceTimer = setTimeout(() => pendingStart?.(), debounceMs);
    },
    { immediate: true },
  );

  function startLookup(epoch: number, url: string, body: string, lookupFn: ComposerLinkPreviewLookup) {
    cancelPendingLookup();
    let rawLookup: ReturnType<ComposerLinkPreviewLookup>;
    try {
      rawLookup = lookupFn(body);
    } catch {
      rawLookup = Promise.reject();
    }
    const lookupSettled = rawLookup.then((result) => {
      const nextState = linkPreviewStateFromLookup(url, result);
      if (epoch === lookupEpoch) {
        state.value = nextState;
      }
      return nextState;
    }).catch(() => {
      const nextState: ComposerLinkPreviewState = { kind: "failed", url };
      if (epoch === lookupEpoch) {
        state.value = nextState;
      }
      return nextState;
    });
    activeLookupSettled = lookupSettled;
    void lookupSettled.finally(() => {
      if (epoch === lookupEpoch && activeLookupSettled === lookupSettled) {
        activeLookupSettled = null;
      }
    });
  }

  return {
    state,
    showCard,
    canDismiss: computed(() => "url" in state.value && state.value.kind !== "dismissed"),
    host,
    title,
    description,
    dismiss,
    sendPayload: computed(() => sendPayloadFromState(state.value)),
    sendPayloadFor,
  };
}

async function settleWithGrace<T>(pending: Promise<T>): Promise<T | undefined> {
  let timer: Parameters<typeof clearTimeout>[0] | undefined;
  const result = await Promise.race<T | undefined>([
    pending,
    new Promise<undefined>((resolve) => {
      timer = setTimeout(resolve, SEND_LOOKUP_GRACE_MS);
    }),
  ]);
  if (timer !== undefined) clearTimeout(timer);
  return result;
}

function stateKey(scope: string | null | undefined, url: string | null): string | null {
  const trimmedScope = scope?.trim();
  return trimmedScope && url ? `${trimmedScope}\n${url}` : null;
}
