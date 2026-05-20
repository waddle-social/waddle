import { onScopeDispose, ref, type Ref } from "vue";

function readFocused(): boolean {
  if (typeof document === "undefined") return true;
  if (document.visibilityState !== "visible") return false;
  return typeof document.hasFocus === "function" ? document.hasFocus() : true;
}

export function useChatWindowVisibility(): { isWindowFocused: Readonly<Ref<boolean>> } {
  const isWindowFocused = ref(readFocused());

  if (typeof window === "undefined") {
    return { isWindowFocused };
  }

  function update() {
    const next = readFocused();
    if (isWindowFocused.value !== next) isWindowFocused.value = next;
  }

  document.addEventListener("visibilitychange", update);
  window.addEventListener("focus", update);
  window.addEventListener("blur", update);
  // iOS Safari bfcache restoration surfaces as `pageshow` (often with
  // `persisted=true`) and may not fire `visibilitychange` in time. Matches
  // the listener set used by `virtual-timeline-measurement.ts`.
  window.addEventListener("pageshow", update);

  onScopeDispose(() => {
    document.removeEventListener("visibilitychange", update);
    window.removeEventListener("focus", update);
    window.removeEventListener("blur", update);
    window.removeEventListener("pageshow", update);
  });

  return { isWindowFocused };
}
