import { adminRoute, type AdminMatch } from "./routes/admin";
import { channelExtensionRoute, type ChannelExtensionMatch } from "./routes/channel-extension";
import { channelRoute, type ChannelMatch } from "./routes/channel";
import { dmRoute, type DmMatch } from "./routes/dm";
import { eventsRoute, type EventsMatch } from "./routes/events";
import { feedRoute, type FeedMatch } from "./routes/feed";
import { homeRoute, type HomeMatch } from "./routes/home";
import { settingsRoute, type SettingsMatch } from "./routes/settings";
import { storiesRoute, type StoriesMatch } from "./routes/stories";
import { threadsRoute, type ThreadsMatch } from "./routes/threads";

export const routes = {
  home: homeRoute,
  channel: channelRoute,
  channelExtension: channelExtensionRoute,
  dm: dmRoute,
  feed: feedRoute,
  stories: storiesRoute,
  events: eventsRoute,
  threads: threadsRoute,
  settings: settingsRoute,
  admin: adminRoute,
} as const;

export type RouteMatch =
  | HomeMatch
  | ChannelMatch
  | ChannelExtensionMatch
  | DmMatch
  | FeedMatch
  | StoriesMatch
  | EventsMatch
  | ThreadsMatch
  | SettingsMatch
  | AdminMatch;

export { type AdminPanel, ADMIN_PANELS, DEFAULT_ADMIN_PANEL } from "./routes/admin";
