/** An image in a resolved lightbox view, with the URL already
 * resolved to a blob URL (decrypted for encrypted attachments) or
 * a plain https URL. Matches `LightboxImage` in ImageLightbox.vue
 * (url, name?, width?, height?). */
export interface ResolvedLightboxImage {
  url: string;
  name?: string;
  width?: number;
  height?: number;
}
