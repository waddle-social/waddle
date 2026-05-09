import type { DiscoveredExtensionCommand } from "./xmpp/extension-commands";

interface SlashMatchContext {
  inMuc: boolean;
}

export function filterSlashCandidates(
  prefix: string,
  commands: DiscoveredExtensionCommand[],
  context: SlashMatchContext,
): DiscoveredExtensionCommand[] {
  const needle = prefix.toLowerCase();
  return commands.filter((command) => {
    if (!command.composerPrefix) return false;
    if (command.scope === "channel" && !context.inMuc) return false;
    return command.composerPrefix.toLowerCase().startsWith(needle);
  });
}

export function resolveSlashCommand(
  prefix: string,
  commands: DiscoveredExtensionCommand[],
  context: SlashMatchContext,
): DiscoveredExtensionCommand | null {
  if (!prefix) return null;
  const needle = prefix.toLowerCase();
  const matches = commands.filter((command) => {
    if (!command.composerPrefix) return false;
    if (command.scope === "channel" && !context.inMuc) return false;
    return command.composerPrefix.toLowerCase() === needle;
  });
  // Multiple extensions advertised the same prefix: refuse to auto-resolve
  // so the user picks explicitly from the autocomplete popover.
  return matches.length === 1 ? matches[0] : null;
}
