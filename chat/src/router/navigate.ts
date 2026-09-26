import { adminRoute } from "./routes/admin";
import { channelExtensionRoute } from "./routes/channel-extension";
import { channelRoute } from "./routes/channel";
import { dmListRoute } from "./routes/dm-list";
import { dmRoute } from "./routes/dm";
import { eventsRoute } from "./routes/events";
import { feedRoute } from "./routes/feed";
import { groupDmRoomRoute } from "./routes/group-dm-room";
import { homeRoute } from "./routes/home";
import { membersRoute } from "./routes/members";
import { roomsRoute } from "./routes/rooms";
import { settingsRoute } from "./routes/settings";
import { storiesRoute } from "./routes/stories";
import { threadsRoute } from "./routes/threads";
import { unreadRoute } from "./routes/unread";
import { currentMatch } from "./use-route-match";
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
    case "groupDmRoom":
      return groupDmRoomRoute.href({ params: match.params, search: match.search });
    case "dmList":
      return dmListRoute.href();
    case "feed":
      return feedRoute.href();
    case "stories":
      return storiesRoute.href();
    case "events":
      return eventsRoute.href();
    case "threads":
      return threadsRoute.href();
    case "unread":
      return unreadRoute.href();
    case "rooms":
      return roomsRoute.href();
    case "members":
      return membersRoute.href();
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
// `match`, and writes `match` into the shared `currentMatch` ref so
// every `useRouteMatch()` consumer (including the controller's
// state-from-match logic) re-renders this tick. `pushState` doesn't
// fire `popstate`, so without the explicit write here the reactive
// match would stay stale until the next back/forward.
//
// No-op when the current URL already equals the target.
export function navigate(match: RouteMatch, opts?: NavigateOptions): void {
  if (typeof window === "undefined") return;
  const href = buildHref(match);
  const current = window.location.pathname + window.location.search;
  if (current === href) {
    currentMatch.value = match;
    return;
  }
  const state: { waddleRouteId: RouteMatch["id"]; origin?: "app" | "direct" } =
    match.id === "settings" && match.origin
      ? { waddleRouteId: "settings", origin: match.origin }
      : { waddleRouteId: match.id };
  if (opts?.replace) {
    window.history.replaceState(state, "", href);
  } else {
    window.history.pushState(state, "", href);
  }
  currentMatch.value = match;
}

/**
 * Settles a per-route Astro island onto the router's match at setup.
 * Astro routes by path shape (`/dm/[peerJid]` accepts any segment) while
 * the router matches by content (`dm` rejects a bare username or a
 * resource without `?scope=occupant`), so an island can mount on a URL
 * whose match already fell back to `home`. Rather than assert and blank
 * the page, put the URL where the match is: replace the history entry
 * with the match's canonical href and let the shell render that page.
 */
export function settleIslandMatch(routeId: RouteMatch["id"]): void {
  if (currentMatch.value.id === routeId) return;
  navigate(currentMatch.value, { replace: true });
}
