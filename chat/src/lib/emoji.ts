import lib from "emojilib";

/**
 * Full emoji shortcode lookup powered by emojilib.
 *
 * emojilib exports Record<emoji, string[]> (emoji -> keywords).
 * We invert it to Record<keyword, emoji> for autocomplete,
 * using the first keyword as the primary shortcode.
 */
const _shortcodes: Record<string, string> = {};
for (const [emoji, keywords] of Object.entries(lib)) {
  for (const kw of keywords) {
    // First keyword wins — don't overwrite
    if (!_shortcodes[kw]) {
      _shortcodes[kw] = emoji;
    }
  }
}

const EMOJI_SHORTCODES: Record<string, string> = _shortcodes;

/** Search emoji shortcodes, returning up to `limit` matches. */
export function searchEmoji(query: string, limit = 8): { name: string; emoji: string }[] {
  const q = query.toLowerCase();
  if (q.length < 2) return [];
  const results: { name: string; emoji: string }[] = [];
  for (const [name, emoji] of Object.entries(EMOJI_SHORTCODES)) {
    if (name.includes(q)) {
      results.push({ name, emoji });
      if (results.length >= limit) break;
    }
  }
  return results;
}
