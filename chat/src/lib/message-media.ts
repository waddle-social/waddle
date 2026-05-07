const IMAGE_URL_RE = /^https?:\/\/\S+\.(?:gif|png|jpe?g|webp|avif|bmp|svg)(?:[?#]\S*)?$/i;
const VIDEO_URL_RE = /^https?:\/\/\S+\.(?:mp4|webm|mov|m4v|ogv)(?:[?#]\S*)?$/i;
const AUDIO_URL_RE = /^https?:\/\/\S+\.(?:mp3|wav|ogg|oga|m4a|aac|flac)(?:[?#]\S*)?$/i;
const PDF_URL_RE = /^https?:\/\/\S+\.pdf(?:[?#]\S*)?$/i;
const IMAGE_EXTENSION_RE = /\.(?:gif|png|jpe?g|webp|avif|bmp|svg)(?:[?#]\S*)?$/i;
const VIDEO_EXTENSION_RE = /\.(?:mp4|webm|mov|m4v|ogv)(?:[?#]\S*)?$/i;
const AUDIO_EXTENSION_RE = /\.(?:mp3|wav|ogg|oga|m4a|aac|flac)(?:[?#]\S*)?$/i;
const PDF_EXTENSION_RE = /\.pdf(?:[?#]\S*)?$/i;
const GIPHY_URL_RE = /^https?:\/\/(?:media\d*\.giphy\.com|i\.giphy\.com)\//i;
const IMAGE_MEDIA_TYPE_RE = /^image\//i;
const VIDEO_MEDIA_TYPE_RE = /^video\//i;
const AUDIO_MEDIA_TYPE_RE = /^audio\//i;

export function isImageUrl(body: string): boolean {
  const trimmed = body.trim();
  return IMAGE_URL_RE.test(trimmed) || GIPHY_URL_RE.test(trimmed);
}

export function isImageFile(mediaType?: string, url?: string): boolean {
  if (mediaType && IMAGE_MEDIA_TYPE_RE.test(mediaType)) return true;
  if (!url) return false;
  const candidate = url.trim();
  return isImageUrl(candidate) || IMAGE_EXTENSION_RE.test(candidate);
}

export function isVideoFile(mediaType?: string, url?: string): boolean {
  if (mediaType && VIDEO_MEDIA_TYPE_RE.test(mediaType)) return true;
  if (!url) return false;
  const candidate = url.trim();
  return VIDEO_URL_RE.test(candidate) || VIDEO_EXTENSION_RE.test(candidate);
}

export function isAudioFile(mediaType?: string, url?: string): boolean {
  if (mediaType && AUDIO_MEDIA_TYPE_RE.test(mediaType)) return true;
  if (!url) return false;
  const candidate = url.trim();
  return AUDIO_URL_RE.test(candidate) || AUDIO_EXTENSION_RE.test(candidate);
}

export function isPdfFile(mediaType?: string, url?: string): boolean {
  if (mediaType === "application/pdf") return true;
  if (!url) return false;
  const candidate = url.trim();
  return PDF_URL_RE.test(candidate) || PDF_EXTENSION_RE.test(candidate);
}

export function inferredFileDisposition(
  mediaType?: string,
  url?: string,
): "inline" | "attachment" {
  return isImageFile(mediaType, url)
    || isVideoFile(mediaType, url)
    || isAudioFile(mediaType, url)
    || isPdfFile(mediaType, url)
    ? "inline"
    : "attachment";
}
