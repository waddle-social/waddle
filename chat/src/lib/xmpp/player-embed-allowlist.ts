// Mirror of the server-side player-embed allowlist (the security boundary lives
// on the server; this is a client-side defense-in-depth re-check before we ever
// emit an <iframe>). Origins here are the FINAL, post-rewrite embed origins —
// the server rewrites youtube.com -> youtube-nocookie.com before sealing, so
// www.youtube.com is intentionally NOT trusted here.
const ALLOWED_PLAYER_EMBED_ORIGINS: readonly string[] = [
  "https://www.youtube-nocookie.com",
  "https://player.vimeo.com",
];

export function isAllowedPlayerEmbedOrigin(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol !== "https:") return false;
  if (parsed.username || parsed.password) return false;
  return ALLOWED_PLAYER_EMBED_ORIGINS.includes(parsed.origin);
}
