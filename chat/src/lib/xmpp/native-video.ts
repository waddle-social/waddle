// Client-side defense-in-depth for page-advertised native `og:video` previews.
// The server is the trust anchor (it strips client-authored XEP-0511 and
// re-stamps its own), but — exactly as the player-embed path re-checks its
// allowlist before emitting an <iframe> — we re-validate here before emitting a
// <video src>. There is no provider allowlist for native playback (it runs no
// third-party JS); the boundary is https + a supported direct media type.
//
// Mirror of the server-supported direct video MIME set. Progressive containers
// play natively in <video>; HLS (application/vnd.apple.mpegurl + aliases) plays
// natively on Safari and via a lazily-loaded hls.js player elsewhere.
const HLS_MEDIA_TYPES: ReadonlySet<string> = new Set([
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/x-mpegurl",
  "audio/mpegurl",
  "application/mpegurl",
]);
const PLAYABLE_NATIVE_VIDEO_TYPES: ReadonlySet<string> = new Set([
  "video/mp4",
  "video/webm",
  "video/ogg",
  "video/quicktime",
  ...HLS_MEDIA_TYPES,
]);

/// The MIME essence of an og:video:type, stripping media-type parameters
/// (`video/mp4; codecs="…"`), mirroring the server resolver's `.split(';')`.
function mediaTypeEssence(mediaType: string): string {
  return mediaType.split(";")[0]!.trim().toLowerCase();
}

function isPlayableNativeVideoMediaType(mediaType: string): boolean {
  return PLAYABLE_NATIVE_VIDEO_TYPES.has(mediaTypeEssence(mediaType));
}

/** Whether a media type is an HLS manifest (needs native-HLS or hls.js). */
export function isHlsMediaType(mediaType: string): boolean {
  return HLS_MEDIA_TYPES.has(mediaTypeEssence(mediaType));
}

export function isPlayableNativeVideo(url: string, mediaType: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol !== "https:") return false;
  if (parsed.username || parsed.password) return false;
  return isPlayableNativeVideoMediaType(mediaType);
}
