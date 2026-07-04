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

export function systemBandToneClass(tone: string): string {
  if (tone === "success") return "chat-system-band--tone-success";
  if (tone === "danger") return "chat-system-band--tone-danger";
  if (tone === "warning") return "chat-system-band--tone-warning";
  return "";
}

/**
 * Per-kind modifier so the stylesheet can tune layout for specific
 * payload shapes — github-event cards hide their meta-item labels and
 * render as a positional chip strip (branch · commit · event) instead
 * of a labeled K/V grid.
 */
export function systemBandKindClass(kind: string): string {
  return kind ? `chat-system-band--kind-${kind}` : "";
}

/**
 * Branch values look like code (slashes, identifier-style suffixes)
 * and deserve the same tabular-mono treatment commit SHAs get.
 */
export function systemBandMetaValueClass(label: string): string {
  return label === "Commit" || label === "Branch"
    ? "chat-system-band__meta-value--mono"
    : "";
}
