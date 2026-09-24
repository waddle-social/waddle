import type { DiscoveredExtensionCommand } from "./xmpp/extension-commands";
import {
  BUILTIN_SLASH_COMMANDS,
  builtinSlashKeywords,
  findBuiltinSlash,
  resolveBuiltinSlash,
  type BuiltinSlashCommand,
  type BuiltinSlashOutcome,
} from "./slash-builtins";
import { filterSlashCandidates, resolveSlashCommand } from "./slash-match";

interface SlashMatchContext {
  inMuc: boolean;
}

/** One row of the slash popover: a client built-in or a server extension command. */
export type SlashCandidate =
  | { kind: "builtin"; command: BuiltinSlashCommand }
  | { kind: "extension"; command: DiscoveredExtensionCommand };

/** What an exactly-typed `/prefix trailing` runs on submit. */
export type SlashResolution =
  | { kind: "builtin"; command: BuiltinSlashCommand; outcome: BuiltinSlashOutcome }
  | { kind: "extension"; command: DiscoveredExtensionCommand };

/** The command word the candidate expands to (without the leading `/`). */
export function slashCandidateName(candidate: SlashCandidate): string {
  return candidate.kind === "builtin" ? candidate.command.name : candidate.command.composerPrefix ?? "";
}

function builtinMatchesPrefix(command: BuiltinSlashCommand, needle: string): boolean {
  return builtinSlashKeywords(command).some((keyword) => keyword.startsWith(needle));
}

function isShadowedByBuiltin(command: DiscoveredExtensionCommand): boolean {
  return !!command.composerPrefix && findBuiltinSlash(command.composerPrefix) !== null;
}

/**
 * Popover candidates for a typed prefix: matching built-ins first (by name
 * or alias, each listed once), then eligible extension commands. Extension
 * commands whose prefix collides with a built-in name or alias are hidden,
 * since the built-in wins resolution.
 */
export function listSlashCandidates(
  prefix: string,
  commands: DiscoveredExtensionCommand[],
  context: SlashMatchContext,
): SlashCandidate[] {
  const needle = prefix.toLowerCase();
  const builtins: SlashCandidate[] = BUILTIN_SLASH_COMMANDS
    .filter((command) => builtinMatchesPrefix(command, needle))
    .map((command) => ({ kind: "builtin", command }));
  const extensions: SlashCandidate[] = filterSlashCandidates(prefix, commands, context)
    .filter((command) => !isShadowedByBuiltin(command))
    .map((command) => ({ kind: "extension", command }));
  return [...builtins, ...extensions];
}

/**
 * Exact resolution for submit. A built-in name or alias always wins over an
 * extension command with the same prefix; a built-in still missing its
 * required argument resolves to null (so Enter completes rather than sends).
 */
export function resolveSlashTarget(
  prefix: string,
  trailing: string,
  commands: DiscoveredExtensionCommand[],
  context: SlashMatchContext,
): SlashResolution | null {
  const builtin = resolveBuiltinSlash(prefix, trailing);
  if (builtin) {
    return builtin.status === "ready"
      ? { kind: "builtin", command: builtin.command, outcome: builtin.outcome }
      : null;
  }
  const command = resolveSlashCommand(prefix, commands, context);
  return command ? { kind: "extension", command } : null;
}
