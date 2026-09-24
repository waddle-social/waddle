interface ComposerPlaceholderInput {
  slowModeCooldown: number;
  needsForumTitle: boolean;
  isForumChannel: boolean;
  /** Surface-specific override, e.g. `Message bob` in a DM or `Reply…` in a thread. */
  placeholder?: string;
  channelName: string;
}

/** The composer's empty-editor hint; transient states outrank the surface's own wording. */
export function composerPlaceholder(input: ComposerPlaceholderInput): string {
  if (input.slowModeCooldown > 0) return `Slow mode — wait ${input.slowModeCooldown}s`;
  if (input.needsForumTitle) return "Write the opening post";
  if (input.isForumChannel) return "Reply in this topic";
  return input.placeholder ?? `Message #${input.channelName}`;
}
