export interface SlashTriggerMatch {
  prefix: string;
  trailing: string;
}

const SLASH_TRIGGER_PATTERN = /^\/([a-zA-Z][a-zA-Z0-9_-]*)?(?:(\s)([\s\S]*))?$/;

export function parseSlashTrigger(text: string): SlashTriggerMatch | null {
  const match = SLASH_TRIGGER_PATTERN.exec(text);
  if (!match) return null;
  const [, rawPrefix, separator, rest] = match;
  const prefix = rawPrefix ?? "";
  if (separator === undefined) return { prefix, trailing: "" };
  return { prefix, trailing: (rest ?? "").trimStart() };
}
