import { adminRoute } from "./routes/admin";
import { channelExtensionRoute } from "./routes/channel-extension";
import { channelRoute } from "./routes/channel";
import { dmRoute } from "./routes/dm";
import { eventsRoute } from "./routes/events";
import { feedRoute } from "./routes/feed";
import { homeRoute } from "./routes/home";
import { settingsRoute } from "./routes/settings";
import { storiesRoute } from "./routes/stories";
import { threadsRoute } from "./routes/threads";
import type { RouteMatch } from "./registry";

// Order matters: longer-prefix routes must be tried before their
// shorter prefixes. `channelExtension` (`/r/:c/x/:p/:r`) must beat
// `channel` (`/r/:c`); `home` (`/` only) sits last and `matchLocation`
// falls back to it for anything that didn't match a real route.
const ORDER = [
  channelExtensionRoute,
  channelRoute,
  dmRoute,
  feedRoute,
  storiesRoute,
  eventsRoute,
  threadsRoute,
  settingsRoute,
  adminRoute,
  homeRoute,
] as const;

export function matchLocation(pathname: string, searchString: string = ""): RouteMatch {
  for (const route of ORDER) {
    const m = route.tryParse(pathname, searchString);
    if (m) return m;
  }
  return { id: "home" };
}
