import { fetchPastedGif } from "./fetch-pasted-gif";
import type { ComposerPastePlan } from "./plan-composer-paste";

/**
 * Asynchronous half of a composer paste: turn a plan into the files to
 * attach, or — when an animated GIF could not be fetched and there is no
 * static fallback image — the GIF URL to insert as text.
 *
 * Resolves to empty `files` when `signal` aborts (e.g. the composer
 * unmounted), so nothing is attached after the fact.
 */

export type ResolvableComposerPastePlan = Exclude<ComposerPastePlan, { kind: "none" }>;

export type ComposerPasteResult =
  | { kind: "files"; files: File[] }
  | { kind: "text"; text: string };

export interface ResolveComposerPasteDeps {
  signal?: AbortSignal;
  fetchGif?: (url: string, options: { signal?: AbortSignal }) => Promise<File | null>;
}

export async function resolveComposerPaste(
  plan: ResolvableComposerPastePlan,
  deps: ResolveComposerPasteDeps = {},
): Promise<ComposerPasteResult> {
  if (plan.kind === "files") return { kind: "files", files: plan.files };
  const fetchGif = deps.fetchGif ?? fetchPastedGif;
  const gif = await fetchGif(plan.url, { signal: deps.signal });
  if (deps.signal?.aborted) return { kind: "files", files: [] };
  if (gif) return { kind: "files", files: [gif] };
  if (plan.fallback) return { kind: "files", files: [plan.fallback] };
  return { kind: "text", text: plan.url };
}
