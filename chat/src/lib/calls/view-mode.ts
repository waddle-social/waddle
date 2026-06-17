/**
 * View mode for the in-call stage: an equal-weight Gallery grid, or a
 * Speaker layout with one large tile following the active speaker.
 */
export type CallViewMode = "gallery" | "speaker";

/**
 * The sticky promotion the Speaker layout uses to pick its large tile:
 * the identity currently promoted to the large tile, or `null` before
 * anyone has spoken.
 */
export type SpeakerPromotionState = {
  readonly promotedIdentity: string | null;
};

export function emptySpeakerPromotion(): SpeakerPromotionState {
  return { promotedIdentity: null };
}

export type AdvanceSpeakerPromotionInput = {
  state: SpeakerPromotionState;
  /** Identities currently (held) reported as actively speaking. */
  activeIdentities: ReadonlySet<string>;
};

export function advanceSpeakerPromotion(
  input: AdvanceSpeakerPromotionInput,
): SpeakerPromotionState {
  const current = input.state.promotedIdentity;
  // Stay on the current speaker while they are still active — a co-speaker
  // joining in must not snatch the large tile from whoever already has it —
  // and through a silence, so the large tile holds the last speaker rather
  // than going blank until someone else talks.
  if (current !== null && (input.activeIdentities.size === 0 || input.activeIdentities.has(current))) {
    return input.state;
  }
  const next = firstOf(input.activeIdentities);
  return { promotedIdentity: next };
}

function firstOf(identities: ReadonlySet<string>): string | null {
  for (const identity of identities) return identity;
  return null;
}
