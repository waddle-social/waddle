import type { DiscoveredExtensionCommand } from "./xmpp/extension-commands";

export type SlashInvocation =
  | {
      kind: "inline-submit";
      command: DiscoveredExtensionCommand;
      fieldName: string;
      value: string;
    }
  | {
      kind: "open-palette";
      command: DiscoveredExtensionCommand;
      prefillFirstRequired?: string;
    }
  | {
      kind: "direct-execute";
      command: DiscoveredExtensionCommand;
    };

export function buildSlashInvocation(
  command: DiscoveredExtensionCommand,
  trailing: string,
): SlashInvocation {
  const value = trailing.trim();
  if (command.inlineField && value.length > 0) {
    return { kind: "inline-submit", command, fieldName: command.inlineField, value };
  }
  if (command.composerExecute && value.length === 0) {
    return { kind: "direct-execute", command };
  }
  // Preserve unexpected trailing text by opening the palette with a prefill.
  if (!command.inlineField && value.length > 0) {
    return { kind: "open-palette", command, prefillFirstRequired: value };
  }
  return { kind: "open-palette", command };
}
