// Client-side defense-in-depth for page-advertised native `og:video` previews.
// The server is the trust anchor (it strips client-authored XEP-0511 and
// re-stamps its own), but — exactly as the player-embed path re-checks its
// allowlist before emitting an <iframe> — we re-validate here before emitting a
// <video src>. There is no provider allowlist for native playback (it runs no
// third-party JS); the boundary is https + a supported direct media type.
//
// Mirror of the server-supported direct video MIME set. HLS
// (application/vnd.apple.mpegurl) plays via a lazily-loaded hls.js player in a
// follow-up; until then only progressive containers are accepted.
const PLAYABLE_NATIVE_VIDEO_TYPES: ReadonlySet<string> = new Set([
  "video/mp4",
  "video/webm",
  "video/ogg",
  "video/quicktime",
]);

function isPlayableNativeVideoMediaType(mediaType: string): boolean {
  // Match on the MIME essence, stripping media-type parameters
  // (`video/mp4; codecs="…"`), mirroring the server resolver's
  // `.split(';').next()` treatment.
  const essence = mediaType.split(";")[0]!.trim().toLowerCase();
  return PLAYABLE_NATIVE_VIDEO_TYPES.has(essence);
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
