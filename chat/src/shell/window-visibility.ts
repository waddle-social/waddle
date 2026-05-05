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

  onScopeDispose(() => {
    document.removeEventListener("visibilitychange", update);
    window.removeEventListener("focus", update);
    window.removeEventListener("blur", update);
  });

  return { isWindowFocused };
}
