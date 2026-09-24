/**
 * Decide whether an `<img src>` copied from a web page points at an
 * animated GIF worth fetching instead of the browser's rasterized,
 * static clipboard image.
 *
 * Only https URLs qualify. Giphy media hosts serve every rendition as
 * `.gif` as well as `.webp`, so a copied `.webp` rendition is rewritten
 * to its `.gif` sibling; Tenor media hosts and any `.gif` path are
 * accepted as-is.
 */

const GIPHY_MEDIA_HOST = /^(?:i|media\d*)\.giphy\.com$/;
const TENOR_MEDIA_HOST = /^(?:c|media\d*)\.tenor\.com$/;

export function pastedAnimatedGifUrl(src: string): string | null {
  const url = parseHttpsUrl(src);
  if (!url) return null;
  const host = url.hostname.toLowerCase();
  if (GIPHY_MEDIA_HOST.test(host)) return giphyGifRendition(url).toString();
  if (TENOR_MEDIA_HOST.test(host)) return url.toString();
  return url.pathname.toLowerCase().endsWith(".gif") ? url.toString() : null;
}

function parseHttpsUrl(src: string): URL | null {
  try {
    const url = new URL(src.trim());
    return url.protocol === "https:" && !url.username && !url.password ? url : null;
  } catch {
    return null;
  }
}

function giphyGifRendition(url: URL): URL {
  if (!url.pathname.toLowerCase().endsWith(".webp")) return url;
  const next = new URL(url.toString());
  next.pathname = `${url.pathname.slice(0, -".webp".length)}.gif`;
  return next;
}
