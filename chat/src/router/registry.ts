import { adminRoute, type AdminMatch } from "./routes/admin";
import { channelExtensionRoute, type ChannelExtensionMatch } from "./routes/channel-extension";
import { channelRoute, type ChannelMatch } from "./routes/channel";
import { dmRoute, type DmMatch } from "./routes/dm";
import { homeRoute, type HomeMatch } from "./routes/home";
import { settingsRoute, type SettingsMatch } from "./routes/settings";
import { threadsRoute, type ThreadsMatch } from "./routes/threads";

export const routes = {
  home: homeRoute,
  channel: channelRoute,
  channelExtension: channelExtensionRoute,
  dm: dmRoute,
  threads: threadsRoute,
  settings: settingsRoute,
  admin: adminRoute,
} as const;

export type RouteMatch =
  | HomeMatch
  | ChannelMatch
  | ChannelExtensionMatch
  | DmMatch
  | ThreadsMatch
  | SettingsMatch
  | AdminMatch;

export { type AdminPanel, ADMIN_PANELS, DEFAULT_ADMIN_PANEL } from "./routes/admin";
