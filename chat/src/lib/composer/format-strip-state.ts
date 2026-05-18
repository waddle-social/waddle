import { ref } from "vue";

/**
 * Session-sticky toggle for the narrow-viewport formatting strip.
 *
 * Shared across all `MessageComposer` instances mounted in the same tab so
 * users who expand the strip once don't have to re-expand it when switching
 * channels or opening the thread panel. Resets on reload — not persisted to
 * localStorage; there is no settings UI for it yet.
 */
export const formatStripExpanded = ref(false);
