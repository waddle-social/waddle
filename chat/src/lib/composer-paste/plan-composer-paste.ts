import { clipboardFiles } from "./clipboard-files";
import { readHtmlImageSources } from "./html-image-sources";
import { pastedAnimatedGifUrl } from "./pasted-gif-url";

/**
 * Synchronous decision for a paste into the chat composer.
 *
 * - `none`: ordinary text/HTML paste; the editor handles it.
 * - `files`: attach the pasted files (any type, like Slack).
 * - `animated-gif`: "Copy image" on an animated GIF. The clipboard only
 *   carries a rasterized static frame (`fallback`), so the original GIF
 *   at `url` should be fetched instead.
 */
export type ComposerPastePlan =
  | { kind: "none" }
  | { kind: "files"; files: File[] }
  | { kind: "animated-gif"; url: string; fallback: File | null };

const NO_PASTE_PLAN: ComposerPastePlan = { kind: "none" };

export function planComposerPaste(data: DataTransfer | null): ComposerPastePlan {
  if (!data) return NO_PASTE_PLAN;
  const files = clipboardFiles(data);
  if (files.some(isGifFile)) return { kind: "files", files };

  const html = data.getData("text/html");
  const imageSources = html ? readHtmlImageSources(html) : [];
  const textIsOnlyImage = plainTextOnlyNamesImage(data.getData("text/plain"), imageSources);

  const gifUrl = imageSources.length === 1 ? pastedAnimatedGifUrl(imageSources[0]) : null;
  if (gifUrl && textIsOnlyImage && files.length <= 1 && files.every(isImageFile)) {
    return { kind: "animated-gif", url: gifUrl, fallback: files[0] ?? null };
  }

  if (files.length === 0) return NO_PASTE_PLAN;
  if (isRichTextWithRenderedImage(html, textIsOnlyImage, files)) return NO_PASTE_PLAN;
  return { kind: "files", files };
}

/**
 * Office suites and spreadsheets put text, HTML and a rendered image of
 * the selection on the clipboard together; that paste is text.
 */
function isRichTextWithRenderedImage(
  html: string,
  textIsOnlyImage: boolean,
  files: readonly File[],
): boolean {
  return html !== "" && !textIsOnlyImage && files.every(isImageFile);
}

function plainTextOnlyNamesImage(text: string, imageSources: readonly string[]): boolean {
  const trimmed = text.trim();
  return trimmed === "" || imageSources.includes(trimmed);
}

function isGifFile(file: File): boolean {
  return file.type.toLowerCase() === "image/gif";
}

function isImageFile(file: File): boolean {
  return file.type.toLowerCase().startsWith("image/");
}
