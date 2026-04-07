export interface RouteState {
  waddleId: string | null;
  channelId: string | null;
}

const PREFIX_WADDLE = "/w/";
const PREFIX_CHANNEL = "/c/";

export function parseRoute(pathname: string): RouteState {
  let waddleId: string | null = null;
  let channelId: string | null = null;

  const wIdx = pathname.indexOf(PREFIX_WADDLE);
  if (wIdx !== -1) {
    const rest = pathname.slice(wIdx + PREFIX_WADDLE.length);
    const parts = rest.split("/");
    waddleId = decodeURIComponent(parts[0] || "");
    if (!waddleId) waddleId = null;

    const cIdx = rest.indexOf(PREFIX_CHANNEL.slice(1));
    if (cIdx !== -1) {
      const afterC = rest.slice(cIdx + PREFIX_CHANNEL.length - 1);
      channelId = decodeURIComponent(afterC.split("/")[0] || "");
      if (!channelId) channelId = null;
    }
  }

  return { waddleId, channelId };
}

export function buildPath(waddleId: string | null, channelId: string | null): string {
  if (!waddleId) return "/";
  let path = `${PREFIX_WADDLE}${encodeURIComponent(waddleId)}`;
  if (channelId) {
    path += `${PREFIX_CHANNEL}${encodeURIComponent(channelId)}`;
  }
  return path;
}

export function pushRoute(waddleId: string | null, channelId: string | null) {
  const path = buildPath(waddleId, channelId);
  if (window.location.pathname !== path) {
    window.history.pushState(null, "", path);
  }
}

export function replaceRoute(waddleId: string | null, channelId: string | null) {
  const path = buildPath(waddleId, channelId);
  if (window.location.pathname !== path) {
    window.history.replaceState(null, "", path);
  }
}
