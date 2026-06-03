import type { AdminMatch } from "./routes/admin";
import type { ChannelExtensionMatch } from "./routes/channel-extension";
import type { ChannelMatch } from "./routes/channel";
import type { DmListMatch } from "./routes/dm-list";
import type { DmMatch } from "./routes/dm";
import type { EventsMatch } from "./routes/events";
import type { FeedMatch } from "./routes/feed";
import type { HomeMatch } from "./routes/home";
import type { SettingsMatch } from "./routes/settings";
import type { StoriesMatch } from "./routes/stories";
import type { ThreadsMatch } from "./routes/threads";
import type { UnreadMatch } from "./routes/unread";

export type RouteMatch =
  | HomeMatch
  | ChannelMatch
  | ChannelExtensionMatch
  | DmMatch
  | DmListMatch
  | FeedMatch
  | StoriesMatch
  | EventsMatch
  | ThreadsMatch
  | UnreadMatch
  | SettingsMatch
  | AdminMatch;

export { type AdminMatch, type AdminPanel } from "./routes/admin";
