export interface RouteState {
  waddleId: string | null;
  channelId: string | null;
}

const PREFIX_WADDLE = "/w/";
const PREFIX_CHANNEL = "/c/";

// Parses /w/{waddleId}/c/{channelId} from pathname segments
export function parseRoute(pathname: string): RouteState {
  const segments = pathname.split("/").filter(Boolean);
  let waddleId: string | null = null;
  let channelId: string | null = null;

  for (let i = 0; i < segments.length - 1; i++) {
    if (segments[i] === "w") {
      waddleId = decodeURIComponent(segments[i + 1]!);
    } else if (segments[i] === "c") {
      channelId = decodeURIComponent(segments[i + 1]!);
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
