import type { ManualStatus } from "@/presence/effective-show";

/**
 * Client-local slash commands available in every composer. They add no new
 * wire shapes: `/me` and `/shrug` send ordinary message bodies (XEP-0245
 * for `/me`), the rest only drive local UI (GIF picker, manual presence).
 */
type BuiltinSlashName = "me" | "shrug" | "giphy" | "away" | "active" | "dnd";

export interface BuiltinSlashCommand {
  name: BuiltinSlashName;
  aliases: readonly string[];
  /** Human usage line shown in the popover, e.g. `/giphy [search]`. */
  usage: string;
  description: string;
  /** When set, an empty argument leaves the command incomplete (never sent). */
  requiresArgument?: true;
}

/** Manual presence choices reachable from a built-in slash command. */
type BuiltinPresencePick = Extract<ManualStatus, "away" | "available" | "dnd">;

/**
 * How a sending built-in reshapes the composer document before it is sent:
 * `me` canonicalizes the leading command to the XEP-0245 `/me ` prefix,
 * `shrug` strips the command and appends `¯\_(ツ)_/¯`.
 */
export type BuiltinSendRewrite = "me" | "shrug";

export type BuiltinSlashOutcome =
  | { kind: "send"; rewrite: BuiltinSendRewrite }
  | { kind: "open-gif-picker"; query: string }
  | { kind: "set-presence"; pick: BuiltinPresencePick };

export type BuiltinSlashResolution =
  | { status: "ready"; command: BuiltinSlashCommand; outcome: BuiltinSlashOutcome }
  | { status: "missing-argument"; command: BuiltinSlashCommand };

export const BUILTIN_SLASH_COMMANDS: readonly BuiltinSlashCommand[] = [
  {
    name: "me",
    aliases: [],
    usage: "/me <action>",
    description: "Send an action message, e.g. /me waves",
    requiresArgument: true,
  },
  {
    name: "shrug",
    aliases: [],
    usage: "/shrug [message]",
    description: "Append ¯\\_(ツ)_/¯ to your message",
  },
  {
    name: "giphy",
    aliases: ["gif"],
    usage: "/giphy [search]",
    description: "Search for a GIF",
  },
  {
    name: "away",
    aliases: [],
    usage: "/away",
    description: "Set your status to Away",
  },
  {
    name: "active",
    aliases: [],
    usage: "/active",
    description: "Set your status to Available",
  },
  {
    name: "dnd",
    aliases: [],
    usage: "/dnd",
    description: "Set your status to Do Not Disturb",
  },
];

/** Every (lowercase) name a built-in answers to: its name plus aliases. */
export function builtinSlashKeywords(command: BuiltinSlashCommand): string[] {
  return [command.name, ...command.aliases];
}

/** Case-insensitive exact lookup by name or alias. */
export function findBuiltinSlash(prefix: string): BuiltinSlashCommand | null {
  const needle = prefix.toLowerCase();
  if (!needle) return null;
  return BUILTIN_SLASH_COMMANDS.find((command) => builtinSlashKeywords(command).includes(needle)) ?? null;
}

function outcomeFor(name: BuiltinSlashName, argument: string): BuiltinSlashOutcome {
  switch (name) {
    case "me":
      return { kind: "send", rewrite: "me" };
    case "shrug":
      return { kind: "send", rewrite: "shrug" };
    case "giphy":
      return { kind: "open-gif-picker", query: argument };
    case "away":
      return { kind: "set-presence", pick: "away" };
    case "active":
      return { kind: "set-presence", pick: "available" };
    case "dnd":
      return { kind: "set-presence", pick: "dnd" };
  }
}

/**
 * Resolve a typed `/prefix trailing` against the built-ins. Returns null
 * when no built-in answers to `prefix`. Presence commands ignore any
 * trailing text.
 */
export function resolveBuiltinSlash(prefix: string, trailing: string): BuiltinSlashResolution | null {
  const command = findBuiltinSlash(prefix);
  if (!command) return null;
  const argument = trailing.trim();
  if (command.requiresArgument && !argument) return { status: "missing-argument", command };
  return { status: "ready", command, outcome: outcomeFor(command.name, argument) };
}
