import {
  extensionPresentation,
  type ExtensionAnnotation,
} from "@/lib/chat-ui";

export interface SystemBandCard {
  annotation: ExtensionAnnotation;
  presentation: ReturnType<typeof extensionPresentation>;
}

export function eventBandsFor(annotations: ExtensionAnnotation[] | undefined): SystemBandCard[] {
  return (annotations ?? [])
    .map((annotation) => ({ annotation, presentation: extensionPresentation(annotation) }))
    .filter((card) => card.presentation.intent === "event");
}

/**
 * Render as a system band when an event-intent annotation declares the
 * `chat-bot` surface. The annotation provider (the extension that
 * publishes the event) is the one that knows it's a system-level
 * notification, not a human reply — and it now says so explicitly via
 * `surfaceKind`, which the WASM codec hydrates from the wire-format
 * `surface` attribute on the payload's root element. No author-hat
 * fallbacks, no "any event intent" hacks: trust the declaration.
 */
export function rendersAsSystemBand(bands: SystemBandCard[]): boolean {
  return bands.some((band) => band.annotation.surfaceKind === "chat-bot");
}

/** Commit and branch values read as code. */
export function isMonoBandMeta(label: string): boolean {
  return label === "Commit" || label === "Branch";
}
