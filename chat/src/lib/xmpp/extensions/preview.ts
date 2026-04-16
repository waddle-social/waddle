/**
 * Receiver-side parsing for Waddle link-preview payloads injected by the
 * server (see `server/crates/waddle-xmpp-xep-link-preview`). The server
 * is authoritative for this namespace; clients never emit it. A stray
 * client-authored `<preview>` is stripped by the server before fan-out,
 * but the receiver's offset-based anti-spoof check
 * (`message-parsing.ts::extractLinkPreview`) remains as defense-in-depth.
 *
 * Wire format:
 *   <reference xmlns='urn:xmpp:reference:0' type='data'
 *              begin='...' end='...' uri='https://...'>
 *     <preview xmlns='urn:waddle:link-preview:0' url='https://...'>
 *       <title>...</title>
 *       <description>...</description>
 *       <site-name>...</site-name>
 *       <image src='https://...' width='1200' height='630'/>
 *       <type>article</type>
 *     </preview>
 *   </reference>
 */
import XMLElement from "stanza/jxt/Element";

export const NS_WADDLE_PREVIEW_0 = "urn:waddle:link-preview:0";

const TITLE_MAX = 200;
const DESCRIPTION_MAX = 300;
const SITE_NAME_MAX = 100;
const TYPE_MAX = 50;

export interface WaddleLinkPreview {
  url: string;
  title?: string;
  description?: string;
  siteName?: string;
  type?: string;
  image?: { src: string; width?: string; height?: string };
}

export function importPreview(reference: XMLElement): WaddleLinkPreview | undefined {
  const previewEl = reference.getChild("preview", NS_WADDLE_PREVIEW_0);
  if (!previewEl) return undefined;

  const url = previewEl.getAttribute("url");
  if (!url || !parsesAsUrl(url)) return undefined;

  const preview: WaddleLinkPreview = { url };

  const title = cap(previewEl.getChild("title", NS_WADDLE_PREVIEW_0)?.getText(), TITLE_MAX);
  if (title !== undefined) preview.title = title;

  const description = cap(
    previewEl.getChild("description", NS_WADDLE_PREVIEW_0)?.getText(),
    DESCRIPTION_MAX,
  );
  if (description !== undefined) preview.description = description;

  const siteName = cap(
    previewEl.getChild("site-name", NS_WADDLE_PREVIEW_0)?.getText(),
    SITE_NAME_MAX,
  );
  if (siteName !== undefined) preview.siteName = siteName;

  const type = cap(previewEl.getChild("type", NS_WADDLE_PREVIEW_0)?.getText(), TYPE_MAX);
  if (type !== undefined) preview.type = type;

  const imageEl = previewEl.getChild("image", NS_WADDLE_PREVIEW_0);
  if (imageEl) {
    const src = imageEl.getAttribute("src");
    if (src && isSafeImageUrl(src)) {
      const width = imageEl.getAttribute("width");
      const height = imageEl.getAttribute("height");
      preview.image = {
        src,
        ...(width ? { width } : {}),
        ...(height ? { height } : {}),
      };
    }
  }

  return preview;
}

function cap(raw: string | undefined, max: number): string | undefined {
  if (raw === undefined) return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return undefined;
  return trimmed.length > max ? trimmed.slice(0, max) : trimmed;
}

function parsesAsUrl(raw: string): boolean {
  try {
    new URL(raw);
    return true;
  } catch {
    return false;
  }
}

function isSafeImageUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}
