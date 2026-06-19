/**
 * Identity of a local-camera background effect: the *desired* state the call
 * engine reconciles the live camera's video `TrackProcessor` toward.
 *
 * Modelled as a discriminated union so "no effect" is a distinct value from
 * "blur" or "a specific replacement image" — the reconciler keys every decision
 * on structural equality of these values, never on a stringly-typed flag.
 */

/** A bundled, self-hosted background image the user can pick from the catalog. */
export type BackgroundImageId = "mountain" | "office" | "abstract";

/** Canonical ordered set — render order and iteration order for the catalog. */
export const BACKGROUND_IMAGE_IDS = ["mountain", "office", "abstract"] as const;

/** Narrow untrusted input (persisted prefs) to a known catalog image id. */
function isBackgroundImageId(value: unknown): value is BackgroundImageId {
  return (
    typeof value === "string" && (BACKGROUND_IMAGE_IDS as readonly string[]).includes(value)
  );
}

/**
 * Where a replacement image's bytes come from. Catalog images are self-hosted
 * static assets keyed by id; a custom image is the user's own upload, whose
 * bytes live in the custom-image store and whose `ref` is bumped on every new
 * upload so re-uploading a *different* picture is not mistaken for a no-op.
 */
export type BackgroundImageRef =
  | { source: "catalog"; id: BackgroundImageId }
  | { source: "custom"; ref: string };

/** The background effect applied to the local camera before publish. */
export type BackgroundEffect =
  | { kind: "off" }
  | { kind: "blur" }
  | { kind: "image"; image: BackgroundImageRef };

/** Any effect other than "off" — i.e. one that needs a live video processor. */
export type ActiveBackgroundEffect = Exclude<BackgroundEffect, { kind: "off" }>;

/** The resting value: no processor attached, raw camera published. */
export const BACKGROUND_OFF: BackgroundEffect = { kind: "off" };

/**
 * The `TrackProcessor.name` every Waddle camera-background processor carries.
 * The engine reads `cameraTrack.getProcessor()?.name === this` to verify an
 * effect is genuinely live (an honest signal that survives LiveKit re-running
 * the processor across a device switch), exactly as the AI-noise filter reads
 * its processor name. Distinct from any third-party video processor name.
 */
export const CAMERA_BACKGROUND_PROCESSOR_NAME = "waddle-camera-background";

/** Structural equality, so the reconciler can leave an unchanged effect alone. */
export function sameBackgroundEffect(a: BackgroundEffect, b: BackgroundEffect): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind !== "image" || b.kind !== "image") return true; // both off, or both blur
  return sameImageRef(a.image, b.image);
}

function sameImageRef(a: BackgroundImageRef, b: BackgroundImageRef): boolean {
  if (a.source !== b.source) return false;
  return a.source === "catalog" && b.source === "catalog"
    ? a.id === b.id
    : a.source === "custom" && b.source === "custom" && a.ref === b.ref;
}

/**
 * Narrow a persisted (untrusted) value to a known `BackgroundEffect`, falling
 * back to off for anything malformed — an unknown catalog id, a custom image
 * with no ref, or junk. Keeps a bad localStorage payload from making the engine
 * perpetually try (and fail) to attach something unresolvable.
 */
export function normalizeBackgroundEffect(value: unknown): BackgroundEffect {
  if (typeof value !== "object" || value === null) return BACKGROUND_OFF;
  const obj = value as Record<string, unknown>;
  if (obj.kind === "off") return BACKGROUND_OFF;
  if (obj.kind === "blur") return { kind: "blur" };
  if (obj.kind !== "image") return BACKGROUND_OFF;
  const image = normalizeImageRef(obj.image);
  return image ? { kind: "image", image } : BACKGROUND_OFF;
}

function normalizeImageRef(value: unknown): BackgroundImageRef | null {
  if (typeof value !== "object" || value === null) return null;
  const obj = value as Record<string, unknown>;
  if (obj.source === "catalog") return isBackgroundImageId(obj.id) ? { source: "catalog", id: obj.id } : null;
  if (obj.source === "custom") return typeof obj.ref === "string" && obj.ref.length > 0 ? { source: "custom", ref: obj.ref } : null;
  return null;
}

/**
 * A stable string identity for an active effect, used as the per-effect key of
 * the reconciler's fail-open guard so a single failing effect can be skipped on
 * defensive re-runs without affecting any other.
 */
export function backgroundEffectKey(effect: ActiveBackgroundEffect): string {
  if (effect.kind === "blur") return "blur";
  const image = effect.image;
  return image.source === "catalog"
    ? `image:catalog:${image.id}`
    : `image:custom:${image.ref}`;
}
