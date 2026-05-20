import { adminRoute } from "./routes/admin";
import { channelExtensionRoute } from "./routes/channel-extension";
import { channelRoute } from "./routes/channel";
import { dmRoute } from "./routes/dm";
import { homeRoute } from "./routes/home";
import { settingsRoute } from "./routes/settings";
import { threadsRoute } from "./routes/threads";
import type { RouteMatch } from "./registry";

export function buildHref(match: RouteMatch): string {
  switch (match.id) {
    case "home":
      return homeRoute.href();
    case "channel":
      return channelRoute.href({ params: match.params, search: match.search });
    case "channelExtension":
      return channelExtensionRoute.href({ params: match.params, search: match.search });
    case "dm":
      return dmRoute.href({ params: match.params, search: match.search });
    case "threads":
      return threadsRoute.href();
    case "settings":
      return settingsRoute.href();
    case "admin":
      return adminRoute.href({ params: match.params });
  }
}

interface NavigateOptions {
  replace?: boolean;
}

// Pushes (or replaces) a browser history entry to the canonical URL for
// `match`. No-op when the current URL already equals the target — keeps
// reactive watchers idempotent (this used to be inlined into each
// `push*Route` helper in the old navigation.ts).
export function navigate(match: RouteMatch, opts?: NavigateOptions): void {
  if (typeof window === "undefined") return;
  const href = buildHref(match);
  const current = window.location.pathname + window.location.search;
  if (current === href) return;
  const state: { waddleRouteId: RouteMatch["id"]; origin?: "app" | "direct" } =
    match.id === "settings" && match.origin
      ? { waddleRouteId: "settings", origin: match.origin }
      : { waddleRouteId: match.id };
  if (opts?.replace) {
    window.history.replaceState(state, "", href);
  } else {
    window.history.pushState(state, "", href);
  }
}
