/**
 * The curated catalog of bundled background-replacement images. Each image is a
 * self-hosted static asset served from our own origin under `/backgrounds/`
 * (no third-party CDN), keyed by the `BackgroundImageId` the reconciler carries.
 *
 * Centralised so the settings selector and the processor's image-URL resolver
 * never drift on which images exist or where they live.
 */

import { BACKGROUND_IMAGE_IDS, type BackgroundImageId } from "./effect-id";

/** One catalog image: its id, a human label for the picker, and its asset URL. */
type BackgroundCatalogEntry = {
  id: BackgroundImageId;
  label: string;
  assetPath: string;
};

const LABELS: Readonly<Record<BackgroundImageId, string>> = {
  mountain: "Mountain",
  office: "Office",
  abstract: "Abstract",
};

/** All catalog images in canonical render order. */
export function backgroundCatalog(): BackgroundCatalogEntry[] {
  return BACKGROUND_IMAGE_IDS.map((id) => ({
    id,
    label: LABELS[id],
    assetPath: `/backgrounds/${id}.png`,
  }));
}

/** The catalog entry for a single image id. */
export function catalogEntry(id: BackgroundImageId): BackgroundCatalogEntry {
  return { id, label: LABELS[id], assetPath: `/backgrounds/${id}.png` };
}
