import { shallowRef, type Ref, type ShallowRef } from "vue";
import { matchLocation } from "./match";
import type { RouteMatch } from "./registry";

// Module-level reactive view of the active route match. There's exactly
// one source of truth across the whole app:
//
//   - `navigate(match)` writes here directly (pushState path).
//   - A popstate listener syncs here on back/forward.
//   - `useRouteMatch()` just returns this ref.
//
// Sharing a single ref across separate Vue islands (AppShell, the
// per-route page, etc.) is correct because Vue's reactivity is keyed on
// the Ref object identity — every importer sees the same instance and
// re-renders on writes.
export const currentMatch: ShallowRef<RouteMatch> = shallowRef(
  typeof window !== "undefined"
    ? matchLocation(window.location.pathname, window.location.search)
    : { id: "home" },
);

if (typeof window !== "undefined") {
  window.addEventListener("popstate", () => {
    currentMatch.value = matchLocation(
      window.location.pathname,
      window.location.search,
    );
  });
}

// Reactive view of the active route match. Returns the shared
// module-level ref — it's already kept in sync with `window.location`
// (initialized at module load, updated on popstate, written by
// `navigate()`), so callers don't need to pass an SSR initial value.
export function useRouteMatch(): Readonly<Ref<RouteMatch>> {
  return currentMatch;
}
