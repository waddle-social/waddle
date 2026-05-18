/**
 * Normalize a user-entered URL into a value safe to apply as a link href.
 *
 * Returns the canonical URL string when the input parses to an http(s) or
 * mailto URL; returns `null` when the input is empty or carries an unsupported
 * scheme. Used by both the persistent link popover in the composer and the
 * bubble-menu link affordance.
 */
export function sanitizeLinkUrl(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return null;
  try {
    const parsed = new URL(trimmed);
    if (!["http:", "https:", "mailto:"].includes(parsed.protocol)) return null;
    return parsed.toString();
  } catch {
    return null;
  }
}
