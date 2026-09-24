/**
 * XEP-0245 `/me` command. The body is sent as-is; a receiver matches the
 * exact string "/me " (case-sensitive, including the space) in the first
 * four characters of the body and presents "* <sender> <action>".
 */
export const ME_COMMAND_PREFIX = "/me ";

/** Length of `ME_COMMAND_PREFIX` in code points (== UTF-16 units: all ASCII). */
export const ME_COMMAND_PREFIX_LENGTH = ME_COMMAND_PREFIX.length;

export interface MeAction {
  /** The verb phrase after "/me " (may be empty). */
  action: string;
}

export function parseMeAction(body: string): MeAction | null {
  if (!body.startsWith(ME_COMMAND_PREFIX)) return null;
  return { action: body.slice(ME_COMMAND_PREFIX_LENGTH) };
}

/** Plain-text preview of a body: `* Sender action` for `/me` bodies, else the body. */
export function formatMePreview(body: string, sender: string): string {
  const me = parseMeAction(body);
  if (!me) return body;
  return me.action ? `* ${sender} ${me.action}` : `* ${sender}`;
}
