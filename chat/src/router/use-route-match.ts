import { computed, onMounted, onUnmounted, ref, type ComputedRef, type Ref } from "vue";
import { matchLocation } from "./match";
import type { RouteMatch } from "./registry";

// Reactive view of the active route match. Initial value is taken from
// `initial` (passed by Astro via the layout — known at SSR time) or, as
// a fallback, parsed from `window.location` on the client. A `popstate`
// listener keeps the ref in sync with back/forward navigation and any
// `navigate()` call that updates `window.history`.
//
// Not exported from `@/router` directly — per-route pages call
// `useTypedMatch` (which composes this) so the match type is narrowed
// to the active route at the call site.
function useRouteMatch(initial?: RouteMatch): Readonly<Ref<RouteMatch>> {
  const current = ref<RouteMatch>(
    initial ??
      (typeof window !== "undefined"
        ? matchLocation(window.location.pathname, window.location.search)
        : { id: "home" }),
  );
  const onPopState = () => {
    current.value = matchLocation(window.location.pathname, window.location.search);
  };
  onMounted(() => {
    if (typeof window === "undefined") return;
    window.addEventListener("popstate", onPopState);
  });
  onUnmounted(() => {
    if (typeof window === "undefined") return;
    window.removeEventListener("popstate", onPopState);
  });
  return current;
}

// Narrows the active match to a specific route id. Throws **at setup
// time** if the active match isn't the expected route — a hard guarantee
// that this composable is only used inside the route's Astro page.
// (The returned computed also throws on reactive route changes, so a
// stale per-route page can't outlive its match.)
export function useTypedMatch<Id extends RouteMatch["id"]>(
  id: Id,
  initial?: RouteMatch,
): ComputedRef<Extract<RouteMatch, { id: Id }>> {
  const match = useRouteMatch(initial);
  if (match.value.id !== id) {
    throw new Error(
      `useTypedMatch(${JSON.stringify(id)}) called on a "${match.value.id}" match`,
    );
  }
  return computed(() => {
    if (match.value.id !== id) {
      throw new Error(
        `useTypedMatch(${JSON.stringify(id)}) called on a "${match.value.id}" match`,
      );
    }
    return match.value as Extract<RouteMatch, { id: Id }>;
  });
}
