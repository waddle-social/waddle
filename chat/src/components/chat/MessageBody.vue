<script setup lang="ts">
// Presentational body + attachments + extension-annotation cards for
// a TimelineMessage. Used by `MessageCard` (timeline) and `PinnedPanel`
// (right rail). `compact` adapts layout for the narrow rail width and
// suppresses interactive affordances that belong on the timeline only
// (extension action buttons).
//
import { computed, inject, reactive, ref, nextTick, watch } from "vue";
import {
  Lock,
  AlertCircle,
  CheckCircle2,
  LoaderCircle,
  MessageSquare,
  LayoutDashboard,
  FileDown,
  Github,
  ExternalLink,
  Play,
} from "lucide-vue-next";
import {
  extensionPresentation,
  extensionSurfaceLabel,
  linkPreviewMediaState,
  renderStyledBody,
  type LinkPreviewMediaState,
  type TimelineMessage,
  type ExtensionAnnotation,
  type ExtensionAnnotationAction,
} from "@/lib/chat-ui";
import { roomLinksKey } from "@/lib/room-links";
import { formatFileSize, useMessageAttachments } from "@/channels/message-attachments";
import { applyShikiToCodeBlocks } from "@/lib/shiki";
import { parseMeAction } from "@/lib/me-command";
import { renderMeActionHtml } from "@/lib/me-action-render";
import { isAllowedPlayerEmbedOrigin } from "@/lib/xmpp/player-embed-allowlist";
import { useExtensionAnnotationActions } from "@/channels/extension-annotation-actions";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import { css, cx } from "styled-system/css";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";
import { remoteMediaPolicyKey } from "@/lib/remote-media";
import { isMonoBandMeta } from "./message-system-band";
import { actionChipClasses as chips, bandClasses as band } from "./message-band-classes";
import HlsVideoPlayer from "@/components/chat/HlsVideoPlayer.vue";

const props = withDefaults(
  defineProps<{
    message: TimelineMessage;
    compact?: boolean;
    invokeExtensionAction?: (action: ExtensionAnnotationAction) => Promise<ExtensionCommandResult>;
  }>(),
  {
    compact: false,
  },
);

const messageRef = computed(() => props.message);
const invokeExtensionActionRef = computed(() => props.invokeExtensionAction);

// Media of a message from another server is fetched only once the user
// asks (see `@/lib/remote-media`). Without a policy, media loads as before.
const remoteMediaPolicy = inject(remoteMediaPolicyKey, () => null);
const remoteMediaHost = computed(() => remoteMediaPolicy(props.message));
const remoteMediaRequested = ref(false);
watch(() => props.message.id, () => { remoteMediaRequested.value = false; });
const mediaShown = computed(() => !remoteMediaHost.value || remoteMediaRequested.value);

const {
  imageAttachments,
  videoAttachments,
  audioAttachments,
  pdfAttachments,
  downloadableAttachments,
  displayBody,
  isInlineImageBodyUrl,
  lightboxOpen,
  lightboxIndex,
  lightboxImages,
  attachmentKey,
  attachmentRenderKey,
  resolvedAttachmentUrl,
  attachmentError,
  isDecryptingAttachment,
  openLightbox,
  openInlineImageLightbox,
  downloadAttachment,
} = useMessageAttachments(messageRef, { mediaAllowed: mediaShown });

const extensionAnnotations = computed(() => props.message.extensionAnnotations ?? []);
const linkPreviews = computed(() => props.message.linkPreviews ?? []);
type MessageLinkPreview = NonNullable<TimelineMessage["linkPreviews"]>[number];
interface LinkPreviewCard {
  preview: MessageLinkPreview;
  mediaState: LinkPreviewMediaState;
}
const linkPreviewCards = computed<LinkPreviewCard[]>(() =>
  linkPreviews.value.map((preview) => ({
    preview,
    mediaState: linkPreviewMediaState(preview),
  })),
);
// Info cards (text/image/unavailable) keep the whole-card anchor; video cards
// render separately because they carry an interactive play control that must
// not also navigate the link.
const linkInfoCards = computed(() =>
  linkPreviewCards.value.filter(
    (card) => card.mediaState.kind !== "video" && card.mediaState.kind !== "player",
  ),
);
const videoPreviewCards = computed(() =>
  linkPreviews.value.flatMap((preview) => {
    const state = linkPreviewMediaState(preview);
    // Defensive: only ever play from an https source, independent of the
    // upstream host policy that already enforces it server-side.
    if (state.kind !== "video" || !isHttpsUrl(state.video.url)) return [];
    return [{ preview, video: state.video, ...(state.poster ? { poster: state.poster } : {}) }];
  }),
);
function isHttpsUrl(value: string): boolean {
  try {
    return new URL(value).protocol === "https:";
  } catch {
    return false;
  }
}
function videoCardKey(preview: MessageLinkPreview): string {
  return `${preview.originalUrl}:${preview.normalizedUrl ?? ""}`;
}
// Native-video playback is local UI state only — never synchronized over XMPP.
// Until the user clicks play, the <video> (owned by HlsVideoPlayer, mounted only
// while playing) and its network fetch are not emitted; a fatal playback error
// flips the card to a link fallback.
const playingVideos = reactive(new Set<string>());
const failedVideos = reactive(new Set<string>());
function startVideoPlayback(preview: MessageLinkPreview): void {
  playingVideos.add(videoCardKey(preview));
}
const playerPreviewCards = computed(() =>
  linkPreviews.value.flatMap((preview) => {
    const state = linkPreviewMediaState(preview);
    // Defense in depth: only ever iframe an allowlisted embed origin.
    if (state.kind !== "player" || !isAllowedPlayerEmbedOrigin(state.player.url)) return [];
    return [{ preview, player: state.player, ...(state.poster ? { poster: state.poster } : {}) }];
  }),
);
const playingEmbeds = reactive(new Set<string>());
function startEmbedPlayback(preview: MessageLinkPreview): void {
  playingEmbeds.add(videoCardKey(preview));
}
const extensionCards = computed(() =>
  extensionAnnotations.value.map((annotation) => ({
    annotation,
    presentation: extensionPresentation(annotation),
  })),
);
// Event-intent cards rendered inline (for the rare case a human-authored
// message also carries event annotations; bot-authored event messages
// are intercepted at the MessageCard level and rendered as full-width
// system bands instead).
const eventCards = computed(() => extensionCards.value.filter((c) => c.presentation.intent === "event"));
// All tool-intent actions collapse into a single horizontal chip strip
// below the body. N extensions × N actions never fans out into N cards.
interface ToolActionChip {
  annotation: ExtensionAnnotation;
  presentation: ReturnType<typeof extensionPresentation>;
  action: ExtensionAnnotationAction;
}
const toolActionChips = computed<ToolActionChip[]>(() => {
  const chips: ToolActionChip[] = [];
  for (const card of extensionCards.value) {
    if (card.presentation.intent !== "tool") continue;
    for (const action of card.annotation.actions) {
      chips.push({ annotation: card.annotation, presentation: card.presentation, action });
    }
  }
  return chips;
});

function linkPreviewHost(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

function linkPreviewHref(url: string): string | null {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" ? parsed.toString() : null;
  } catch {
    return null;
  }
}

const {
  actionState: extensionActionState,
  actionStatusLabel,
  invokeExtension,
} = useExtensionAnnotationActions({
  annotations: extensionAnnotations,
  invokeExtensionAction: invokeExtensionActionRef,
});

const isSticker = computed(() => !!props.message.isSticker && imageAttachments.value.length > 0);

/** What a message from another server would fetch, named instead of loaded. */
const withheldMedia = computed(() => {
  if (mediaShown.value) return [];
  const files = [
    ...imageAttachments.value,
    ...videoAttachments.value,
    ...audioAttachments.value,
    ...pdfAttachments.value,
  ].map((file) => ({ key: attachmentKey(file), label: file.name ?? file.url, url: file.url }));
  const bodyImage = isInlineImageBodyUrl.value
    ? [{ key: "inline-body-image", label: props.message.body.trim(), url: props.message.body.trim() }]
    : [];
  const previewImages = linkPreviewCards.value.some((card) =>
    card.mediaState.kind === "image"
    || ((card.mediaState.kind === "video" || card.mediaState.kind === "player") && !!card.mediaState.poster),
  )
    ? [{ key: "link-preview-images", label: "Link preview pictures", url: "" }]
    : [];
  return [...files, ...bodyImage, ...previewImages];
});

// XEP-0245: a body starting with exactly "/me " renders as an italic
// "* Author action" line. Corrections (XEP-0308) replace `body`, so an
// edited `/me` message follows the same path.
const isMeAction = computed(() => parseMeAction(displayBody.value) !== null);
const roomLinks = inject(roomLinksKey);
function onBodyClick(event: MouseEvent) {
  if (!roomLinks || !(event.target instanceof Element)
    || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
  const link = event.target.closest<HTMLAnchorElement>("a[data-room-jid]");
  const roomJid = link?.dataset.roomJid;
  if (!roomJid) return;
  event.preventDefault();
  event.stopPropagation();
  roomLinks.open(roomJid);
}

const styledHtml = computed(() =>
  isMeAction.value
    ? renderMeActionHtml({
        body: displayBody.value,
        markup: props.message.markup,
        references: props.message.references,
        roomHref: roomLinks?.href,
        actor: props.message.author,
      })
    : renderStyledBody(displayBody.value, props.message.markup, props.message.references, roomLinks?.href),
);
const shouldRenderTextBody = computed(() =>
  !!displayBody.value && !props.message.extensionBodyFallback,
);
const styledBodyRef = ref<HTMLDivElement | null>(null);

watch(styledBodyRef, (body, _previous, onCleanup) => {
  if (!body || !roomLinks) return;
  body.addEventListener("click", onBodyClick);
  onCleanup(() => body.removeEventListener("click", onBodyClick));
});

async function highlightMessageCodeBlocks() {
  const el = styledBodyRef.value;
  if (!el) return;
  await applyShikiToCodeBlocks(el);
}

watch(
  styledHtml,
  () => {
    void nextTick().then(highlightMessageCodeBlocks);
  },
  { immediate: true },
);

// --- Presentation (Polar Huddle). Rich text keeps the editor's rules
// (paragraphs, code, quotes, lists, links) and renders mentions in gold.
const c = {
  stack: css({ display: "flex", width: "100%", minWidth: 0, flexDirection: "column", alignItems: "flex-start", gap: "1.5" }),
  rich: css({
    color: "inherit",
    overflowWrap: "anywhere",
    overflow: "clip",
    overflowClipMargin: "0.25em",
    minWidth: 0,
    width: "100%",
    maxWidth: "100%",
    "& p": { margin: 0 },
    "& p + p": { mt: "2" },
    "& strong, & .font-semibold": { fontWeight: 700 },
    "& em": { fontStyle: "italic" },
    "& del, & s": { textDecoration: "line-through", opacity: 0.6 },
    "& code": { fontFamily: "mono", fontSize: "0.875em", fontWeight: 400, bg: "surface.code", padding: "1px {spacing.1}", borderRadius: "tag" },
    "& pre": {
      fontFamily: "mono",
      fontSize: "sm",
      lineHeight: 1.5,
      padding: "{spacing.2.5} {spacing.4}",
      my: "2",
      maxWidth: "100%",
      overflowX: "auto",
      bg: "surface.code",
      border: "1px solid {colors.border.soft}",
      borderRadius: "control",
    },
    "& pre code": { padding: 0, bg: "transparent", borderRadius: 0, fontSize: "inherit" },
    "& pre.shiki, & pre.shiki span": {
      color: "light-dark(var(--shiki-light), var(--shiki-dark)) !important",
      backgroundColor: "transparent !important",
    },
    "& pre.shiki": { backgroundColor: "light-dark(var(--shiki-light-bg), var(--shiki-dark-bg)) !important" },
    "& blockquote": { my: "2", pl: "3", borderLeft: "3px solid {colors.border.strong}", color: "fg.muted" },
    "& ul, & ol": { my: "1", pl: "1.4em" },
    "& ul": { listStyleType: "disc" },
    "& ol": { listStyleType: "decimal" },
    "& li": { my: "0.5" },
    "& li > p": { margin: 0 },
    "& a": {
      color: "action.text",
      textDecoration: "underline",
      textDecorationColor: "transparent",
      textUnderlineOffset: "2px",
      transition: "text-decoration-color {durations.fast} {easings.out}, color {durations.fast} {easings.out}",
      _hover: { color: "fg", textDecorationColor: "currentColor" },
    },
    "& .rich-mention": { color: "gold.text", bg: "gold.subtle", fontWeight: 700, padding: "0 {spacing.1}", borderRadius: "tag" },
  }),
  text: css({ textStyle: "message" }),
  compact: css({ textStyle: "body", lineClamp: 3 }),
  me: css({ fontStyle: "italic" }),
  sticker: css({ borderRadius: "control", objectFit: "contain", maxWidth: "112px", maxHeight: "112px" }),
  stickerCompact: css({ borderRadius: "control", objectFit: "contain", maxWidth: "80px", maxHeight: "80px" }),
  placeholder: css({
    display: "flex",
    flexDirection: "column",
    alignItems: "center",
    justifyContent: "center",
    gap: "2",
    px: "4",
    textAlign: "center",
    textStyle: "caption",
    color: "fg.muted",
    "& > svg": { width: "16px", height: "16px", color: "action.text" },
  }),
  dashed: css({ border: "1px dashed {colors.border}", borderRadius: "control" }),
  size112: css({ width: "112px", height: "112px" }),
  size80: css({ width: "80px", height: "80px" }),
  thumb: css({ width: "80px", height: "80px", objectFit: "cover" }),
  imageBox: css({ width: "192px", height: "144px" }),
  h40: css({ width: "100%", height: "160px" }),
  h20: css({ width: "100%", minHeight: "80px" }),
  inlineImage: css({ borderRadius: "control", border: "1px solid {colors.border.soft}", cursor: "pointer", transition: "opacity {durations.fast} {easings.out}", _hover: { opacity: 0.9 } }),
  inlineImageCompact: css({ maxHeight: "96px" }),
  attachmentImage: css({
    maxWidth: "100%",
    maxHeight: "256px",
    aspectRatio: "var(--image-width) / var(--image-height)",
    width: "min(320px, var(--image-width) * 1px, 256px * var(--image-width) / var(--image-height))",
  }),
  linkCard: css({
    display: "flex",
    flexDirection: "column",
    gap: "1",
    width: "100%",
    maxWidth: "576px",
    p: "3",
    textAlign: "left",
    color: "fg",
    textDecoration: "none",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    borderLeft: "3px solid {colors.border.strong}",
    borderRadius: "control",
    transition: "border-color {durations.fast} {easings.out}, background-color {durations.fast} {easings.out}",
  }),
  linkCardHover: css({ _hover: { borderColor: "border", borderLeftColor: "action.text", bg: "surface.2" } }),
  host: css({ display: "flex", alignItems: "center", gap: "1", textStyle: "caption", color: "fg.muted", "& svg": { width: "14px", height: "14px" } }),
  previewImage: css({ width: "100%", borderRadius: "sm", border: "1px solid {colors.border.soft}", objectFit: "cover" }),
  previewImageSized: css({ maxHeight: "192px" }),
  previewImageFixed: css({ height: "192px" }),
  unavailable: css({
    display: "flex",
    alignItems: "center",
    gap: "1",
    px: "2",
    py: "1",
    textStyle: "caption",
    color: "fg.muted",
    border: "1px dashed {colors.border}",
    borderRadius: "sm",
    "& svg": { width: "14px", height: "14px" },
  }),
  linkTitle: css({ fontSize: "sm", fontWeight: 700, color: "fg", textDecoration: "none", "a&:hover": { textDecoration: "underline" } }),
  linkDescription: css({ textStyle: "caption", color: "fg.muted", lineClamp: 2 }),
  media: css({
    position: "relative",
    display: "flex",
    width: "100%",
    aspectRatio: "16 / 9",
    alignItems: "center",
    justifyContent: "center",
    gap: "2",
    overflow: "hidden",
    borderRadius: "sm",
    border: "1px solid {colors.border.soft}",
    bg: "material.media",
    color: "ice.50",
    fontSize: "sm",
    textDecoration: "none",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "surface.code" },
    "& > img": { position: "absolute", inset: 0, width: "100%", height: "100%", objectFit: "cover" },
    "& > svg": { position: "relative", width: "40px", height: "40px", filter: "drop-shadow(0 1px 2px rgb(0 0 0 / 0.5))" },
    "& > svg:first-child": { width: "20px", height: "20px", filter: "none" },
  }),
  embed: css({ width: "100%", borderRadius: "sm", border: "1px solid {colors.border.soft}", bg: "surface.code" }),
  embedRatio: css({ aspectRatio: "16 / 9" }),
  strip: css({ display: "flex", flexWrap: "wrap", gap: "2" }),
  imageTile: css({ maxWidth: "min(100%, 322px)", overflow: "hidden", borderRadius: "control", border: "1px solid {colors.border.soft}", bg: "surface" }),
  cover: css({ objectFit: "cover" }),
  contain: css({ objectFit: "contain" }),
  imageButton: css({
    display: "block",
    maxWidth: "100%",
    cursor: "zoom-in",
    transition: "opacity {durations.fast} {easings.out}",
    _hover: { opacity: 0.9 },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "-3px" },
  }),
  encryptedTag: css({
    display: "flex",
    alignItems: "center",
    gap: "1",
    px: "2",
    py: "1",
    borderTop: "1px solid {colors.border.soft}",
    textStyle: "caption",
    fontWeight: 600,
    color: "fg.muted",
    "& svg": { width: "12px", height: "12px", color: "action.text" },
  }),
  smallButton: css({
    px: "2.5",
    py: "1",
    borderRadius: "sm",
    border: "1px solid {colors.border}",
    bg: "bg",
    color: "fg",
    textStyle: "caption",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
  }),
  column: css({ display: "flex", flexDirection: "column", gap: "2" }),
  columnTight: css({ display: "flex", flexDirection: "column", gap: "1.5" }),
  card: css({
    display: "flex",
    flexDirection: "column",
    gap: "2",
    width: "min(100%, 448px)",
    maxWidth: "100%",
    p: "2",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    borderRadius: "control",
  }),
  player: css({ width: "100%", maxHeight: "288px", borderRadius: "sm", border: "1px solid {colors.border.soft}", bg: "surface.code" }),
  audio: css({ width: "100%" }),
  pdf: css({ width: "100%", height: "288px", borderRadius: "sm", border: "1px solid {colors.border.soft}", bg: "bg" }),
  fileMeta: css({ textStyle: "caption", color: "fg.muted" }),
  fileMetaRow: css({ display: "flex", flexWrap: "wrap", alignItems: "center", gap: "1.5", textStyle: "caption", color: "fg.muted" }),
  encryptedInline: css({ display: "inline-flex", alignItems: "baseline", gap: "1", "& svg": { width: "12px", height: "12px", alignSelf: "center", color: "action.text" } }),
  encryptedPill: css({ display: "inline-flex", alignItems: "center", gap: "1", px: "1.5", py: "0.5", borderRadius: "pill", bg: "action.subtle", color: "action.text", "& svg": { width: "12px", height: "12px" } }),
  fileCard: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "3",
    width: "min(100%, 448px)",
    maxWidth: "100%",
    p: "3",
    textAlign: "left",
    color: "fg",
    textDecoration: "none",
    bg: "surface.2",
    border: "1px solid {colors.border.soft}",
    borderRadius: "control",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}, border-color {durations.fast} {easings.out}",
    _hover: { borderColor: "border" , bgImage: "linear-gradient({colors.state.hover}, {colors.state.hover})" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
    "& > svg": { width: "16px", height: "16px", flex: "none", color: "fg.muted" },
  }),
  fileInfo: css({ flex: 1, minWidth: 0 }),
  fileName: css({ fontSize: "sm", fontWeight: 600, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  fileError: css({ textStyle: "caption", color: "danger.text" }),
  fileChip: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    maxWidth: "100%",
    px: "2",
    py: "1",
    textStyle: "caption",
    color: "fg",
    textDecoration: "none",
    border: "1px solid {colors.border}",
    borderRadius: "sm",
    bg: "bg",
    "a&:hover": { bg: "state.hover" },
    "& svg": { width: "12px", height: "12px", flex: "none" },
    "& > span:first-of-type": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
    "&[data-muted]": { bg: "surface.2", color: "fg.muted" },
  }),
  muted: css({ color: "fg.muted" }),
  /** Media from another server, named until the user asks to load it. */
  withheld: css({
    display: "flex",
    flexDirection: "column",
    gap: "1.5",
    width: "min(100%, 448px)",
    maxWidth: "100%",
    p: "3",
    bg: "surface",
    border: "1px dashed {colors.border}",
    borderRadius: "control",
  }),
  withheldList: css({ display: "flex", flexDirection: "column", gap: "0.5", margin: 0, padding: 0, listStyle: "none" }),
  withheldLoad: css({ alignSelf: "flex-start" }),
  truncate: css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
};
</script>

<template>
  <div :class="c.stack">
    <!-- User text body (shown alongside attachments) -->
    <div
      v-if="shouldRenderTextBody"
      ref="styledBodyRef"
      :class="cx(c.rich, compact ? c.compact : c.text, isMeAction && c.me)"
      v-html="styledHtml"
    />

    <!-- Sticker: single image rendered at thumb size, with decryption support.
         v-else-if chains with displayBody and isInlineImageBodyUrl so only one body/media branch
         renders per message (XEP-0449 stickers carry a <body> alt-text that must
         not also render as a text bubble). -->
    <div v-else-if="isSticker && mediaShown">
      <img
        v-if="resolvedAttachmentUrl(imageAttachments[0])"
        :src="resolvedAttachmentUrl(imageAttachments[0])!"
        :alt="imageAttachments[0].desc ?? message.body ?? 'Sticker'"
        :class="compact ? c.stickerCompact : c.sticker"
        loading="lazy"
        @click.stop
      />
      <div
        v-else
        :class="cx(c.placeholder, c.dashed, compact ? c.size80 : c.size112)"
        @click.stop
      >
        <Lock />
        <span>{{ attachmentError(imageAttachments[0]) ?? (isDecryptingAttachment(imageAttachments[0]) ? "Decrypting sticker…" : "Preparing sticker…") }}</span>
      </div>
    </div>

    <!-- Inline image body URL -->
    <div v-else-if="isInlineImageBodyUrl && mediaShown">
      <img
        :src="message.body.trim()"
        alt="Image"
        :class="cx(c.inlineImage, c.contain, compact ? c.inlineImageCompact : c.attachmentImage)"
        loading="lazy"
        @click.stop="openInlineImageLightbox"
      />
    </div>

    <!-- Link previews (XEP-0511): text/image/unavailable cards keep the
         whole-card anchor. -->
    <a
      v-for="{ preview, mediaState } in linkInfoCards"
      :key="`${preview.originalUrl}:${preview.normalizedUrl ?? ''}`"
      :href="linkPreviewHref(preview.originalUrl) ?? undefined"
      target="_blank"
      rel="noopener noreferrer"
      :class="cx(c.linkCard, c.linkCardHover)"
      @click.stop
    >
      <span :class="c.host">
        <ExternalLink aria-hidden="true" />
        {{ linkPreviewHost(preview.originalUrl) }}
      </span>
      <img
        v-if="mediaState.kind === 'image' && mediaShown"
        :src="mediaState.image.url"
        :alt="mediaState.image.alt ?? preview.title ?? 'Link preview image'"
        :width="mediaState.image.width"
        :height="mediaState.image.height"
        loading="lazy"
        decoding="async"
        :class="cx(c.previewImage, mediaState.image.width && mediaState.image.height ? c.previewImageSized : c.previewImageFixed)"
      />
      <span
        v-else-if="mediaState.kind === 'remote-unavailable'"
        :class="c.unavailable"
      >
        <AlertCircle aria-hidden="true" />
        Remote preview media unavailable
      </span>
      <span v-if="preview.title" :class="c.linkTitle">{{ preview.title }}</span>
      <span v-if="preview.description" :class="c.linkDescription">{{ preview.description }}</span>
    </a>

    <!-- Trusted direct-video preview cards. Playback is local-only and starts
         inline only after the user clicks play; the player element (and its
         network fetch) is emitted only after that action. -->
    <div
      v-for="card in videoPreviewCards"
      :key="`video:${videoCardKey(card.preview)}`"
      :class="c.linkCard"
    >
      <span :class="c.host">
        <ExternalLink aria-hidden="true" />
        {{ linkPreviewHost(card.preview.originalUrl) }}
      </span>
      <HlsVideoPlayer
        v-if="playingVideos.has(videoCardKey(card.preview)) && !failedVideos.has(videoCardKey(card.preview))"
        :url="card.video.url"
        :media-type="card.video.mediaType"
        @failed="failedVideos.add(videoCardKey(card.preview))"
      />
      <a
        v-else-if="failedVideos.has(videoCardKey(card.preview))"
        :href="linkPreviewHref(card.preview.originalUrl) ?? undefined"
        target="_blank"
        rel="noopener noreferrer"
        :class="c.media"
        @click.stop
      >
        <AlertCircle aria-hidden="true" />
        Couldn't play here — watch on site
      </a>
      <button
        v-else
        type="button"
        :class="c.media"
        :aria-label="`Play video${card.preview.title ? ': ' + card.preview.title : ''}`"
        @click.stop.prevent="startVideoPlayback(card.preview)"
      >
        <img
          v-if="card.poster && mediaShown"
          :src="card.poster.url"
          :alt="card.poster.alt ?? ''"
          loading="lazy"
          decoding="async"
        />
        <Play aria-hidden="true" />
      </button>
      <a
        :href="linkPreviewHref(card.preview.originalUrl) ?? undefined"
        target="_blank"
        rel="noopener noreferrer"
        :class="c.linkTitle"
        @click.stop
      >{{ card.preview.title ?? card.preview.originalUrl }}</a>
      <span v-if="card.preview.description" :class="c.linkDescription">{{ card.preview.description }}</span>
    </div>

    <!-- Embeddable player previews (allowlisted og:video). Loads the third-party
         iframe only after the user clicks play; the facade shows the
         Waddle-proxied thumbnail and nothing contacts the provider until then. -->
    <div
      v-for="card in playerPreviewCards"
      :key="`player:${videoCardKey(card.preview)}`"
      :class="c.linkCard"
    >
      <span :class="c.host">
        <ExternalLink aria-hidden="true" />
        {{ linkPreviewHost(card.preview.originalUrl) }}
      </span>
      <iframe
        v-if="playingEmbeds.has(videoCardKey(card.preview))"
        :src="card.player.url"
        :title="card.preview.title ?? 'Embedded player'"
        :class="cx(c.embed, !(card.player.width && card.player.height) && c.embedRatio)"
        :style="card.player.width && card.player.height ? { aspectRatio: `${card.player.width} / ${card.player.height}` } : undefined"
        loading="lazy"
        allow="encrypted-media; picture-in-picture; fullscreen"
        allowfullscreen
        referrerpolicy="strict-origin-when-cross-origin"
      />
      <button
        v-else
        type="button"
        :class="c.media"
        :aria-label="`Play video${card.preview.title ? ': ' + card.preview.title : ''}`"
        @click.stop.prevent="startEmbedPlayback(card.preview)"
      >
        <img
          v-if="card.poster && mediaShown"
          :src="card.poster.url"
          :alt="card.poster.alt ?? ''"
          loading="lazy"
          decoding="async"
        />
        <Play aria-hidden="true" />
      </button>
      <a
        :href="linkPreviewHref(card.preview.originalUrl) ?? undefined"
        target="_blank"
        rel="noopener noreferrer"
        :class="c.linkTitle"
        @click.stop
      >{{ card.preview.title ?? card.preview.originalUrl }}</a>
      <span v-if="card.preview.description" :class="c.linkDescription">{{ card.preview.description }}</span>
    </div>

    <!-- Waddle extension annotations. Event-intent (rare on human-authored
         messages; bot-authored events render as full-width system bands
         in MessageCard) renders as a compact inline band. -->
    <section
      v-for="card in eventCards"
      :key="`${card.annotation.extensionId}:${card.annotation.annotationId}`"
      :class="band.root"
      data-inline
      :data-tone="card.presentation.tone || undefined"
      :data-kind="card.presentation.kind || undefined"
    >
      <div :class="band.header">
        <span :class="band.source">
          <MessageSquare v-if="card.annotation.surfaceKind === 'chat-bot'" aria-hidden="true" />
          <Github v-else-if="card.presentation.kind === 'github-event'" aria-hidden="true" />
          <LayoutDashboard v-else aria-hidden="true" />
          {{ card.presentation.label || extensionSurfaceLabel(card.annotation.surfaceKind) }}
        </span>
        <span v-if="card.presentation.primaryValue" :class="band.pill">{{ card.presentation.primaryValue }}</span>
      </div>
      <div :class="band.title">
        <a
          v-if="card.presentation.primaryUrl && !compact"
          :href="card.presentation.primaryUrl"
          target="_blank"
          rel="noopener noreferrer"
          :class="band.titleLink"
          @click.stop
        >
          <span>{{ card.presentation.title }}</span>
          <ExternalLink aria-hidden="true" />
        </a>
        <template v-else>{{ card.presentation.title }}</template>
      </div>
      <div
        v-if="card.presentation.details.length > 0 || card.presentation.secondaryValue"
        :class="band.meta"
      >
        <span v-if="card.presentation.secondaryValue" :class="band.metaItem">
          <span :class="band.metaValue">{{ card.presentation.secondaryValue }}</span>
        </span>
        <span
          v-for="detail in card.presentation.details"
          :key="`${card.annotation.annotationId}:${detail.label}`"
          :class="band.metaItem"
        >
          <span :class="band.metaLabel">{{ detail.label }}</span>
          <span
            :class="band.metaValue"
            :data-mono="isMonoBandMeta(detail.label) || undefined"
            :title="detail.value"
          >{{ detail.value }}</span>
        </span>
      </div>
    </section>

    <!-- Tool-intent extension actions — a single horizontal chip strip
         under the message body. Multiple extensions each contributing
         actions to the same message all collapse here. -->
    <div v-if="!compact && toolActionChips.length > 0" :class="chips.strip">
      <button
        v-for="chip in toolActionChips"
        :key="`${chip.annotation.annotationId}:${chip.action.route}`"
        type="button"
        :class="chips.chip"
        :data-state="extensionActionState(chip.annotation.annotationId, chip.action)?.state"
        :disabled="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'loading' || !chip.action.launch"
        :title="extensionActionState(chip.annotation.annotationId, chip.action)?.detail
          ?? `${chip.presentation.label}: ${chip.action.label}`"
        @click.stop="invokeExtension(chip.annotation.annotationId, chip.action)"
      >
        <LoaderCircle v-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'loading'" aria-hidden="true" />
        <CheckCircle2 v-else-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'success'" aria-hidden="true" />
        <AlertCircle v-else-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'warning'" aria-hidden="true" />
        <AlertCircle v-else-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'error'" aria-hidden="true" />
        <MessageSquare v-else-if="chip.annotation.surfaceKind === 'chat-bot'" aria-hidden="true" />
        <Github v-else-if="chip.presentation.kind === 'github-event'" aria-hidden="true" />
        <LayoutDashboard v-else aria-hidden="true" />
        <span :class="chips.source">{{ chip.presentation.label }}</span>
        <span>{{ chip.action.label }}</span>
      </button>
      <template v-for="chip in toolActionChips" :key="`${chip.annotation.annotationId}:${chip.action.route}:detail`">
        <span
          v-if="extensionActionState(chip.annotation.annotationId, chip.action)?.detail && extensionActionState(chip.annotation.annotationId, chip.action)?.state !== 'success'"
          :class="chips.detail"
          :data-state="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'error' ? 'error' : 'warning'"
        >
          {{ extensionActionState(chip.annotation.annotationId, chip.action)?.detail }}
        </span>
      </template>
    </div>

    <!-- Media from another server, named until the user asks to load it -->
    <div v-if="withheldMedia.length > 0" :class="c.withheld">
      <span :class="c.fileMeta">
        Images and files from {{ remoteMediaHost }} aren't loaded until you choose to.
      </span>
      <ul :class="c.withheldList">
        <li
          v-for="item in withheldMedia"
          :key="item.key"
          :class="[c.fileMeta, c.truncate]"
          :title="item.url || undefined"
        >{{ item.label }}</li>
      </ul>
      <button type="button" :class="[c.smallButton, c.withheldLoad]" @click.stop="remoteMediaRequested = true">
        Load
      </button>
    </div>

    <!-- Image attachments gallery -->
    <div v-if="imageAttachments.length > 0 && !isSticker && mediaShown" :class="c.strip">
      <div
        v-for="img in imageAttachments"
        :key="attachmentRenderKey(img)"
        :class="c.imageTile"
      >
        <button
          v-if="resolvedAttachmentUrl(img)"
          type="button"
          :class="c.imageButton"
          :title="img.name ?? 'Image'"
          @click.stop="openLightbox(img)"
        >
          <img
            :src="resolvedAttachmentUrl(img) || ''"
            :alt="img.name ?? 'Shared image'"
            :class="compact ? c.thumb : cx(c.attachmentImage, c.cover)"
            :style="img.width && img.height ? { '--image-width': img.width, '--image-height': img.height } : undefined"
            loading="lazy"
          />
        </button>
        <div
          v-else
          :class="cx(c.placeholder, compact ? c.size80 : c.imageBox)"
          @click.stop
        >
          <Lock />
          <span>{{ attachmentError(img) ?? (isDecryptingAttachment(img) ? "Decrypting image…" : "Preparing image…") }}</span>
          <button
            v-if="attachmentError(img) && !compact"
            type="button"
            :class="c.smallButton"
            @click.stop="downloadAttachment(img)"
          >
            Download
          </button>
        </div>
        <div
          v-if="img.encrypted"
          :class="c.encryptedTag"
          @click.stop
        >
          <Lock />
          <span>Encrypted</span>
        </div>
      </div>
    </div>

    <ImageLightbox
      v-if="lightboxOpen"
      v-model:open="lightboxOpen"
      v-model:index="lightboxIndex"
      :images="lightboxImages"
    />

    <!-- Inline video attachments -->
    <div v-if="videoAttachments.length > 0 && mediaShown" :class="c.column">
      <div
        v-for="file in videoAttachments"
        :key="attachmentKey(file)"
        :class="c.card"
      >
        <video
          v-if="resolvedAttachmentUrl(file)"
          :src="resolvedAttachmentUrl(file) || ''"
          :class="c.player"
          controls
          playsinline
          :preload="compact ? 'none' : 'metadata'"
          @click.stop
        />
        <div
          v-else
          :class="cx(c.placeholder, c.dashed, c.h40)"
        >
          <Lock />
          <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting video…" : "Preparing video…") }}</span>
          <button
            v-if="attachmentError(file)"
            type="button"
            :class="c.smallButton"
            @click.stop="downloadAttachment(file)"
          >
            Download
          </button>
        </div>
        <div :class="c.fileMeta">
          {{ file.name ?? "Video" }} · {{ file.mediaType ?? "video" }}
          <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
          <span v-if="file.encrypted" :class="c.encryptedInline"> · <Lock aria-hidden="true" /> Encrypted</span>
        </div>
      </div>
    </div>

    <!-- Inline audio attachments -->
    <div v-if="audioAttachments.length > 0 && mediaShown" :class="c.column">
      <div
        v-for="file in audioAttachments"
        :key="attachmentKey(file)"
        :class="c.card"
      >
        <audio
          v-if="resolvedAttachmentUrl(file)"
          :src="resolvedAttachmentUrl(file) || ''"
          :class="c.audio"
          controls
          :preload="compact ? 'none' : 'metadata'"
          @click.stop
        />
        <div
          v-else
          :class="cx(c.placeholder, c.dashed, c.h20)"
        >
          <Lock />
          <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting audio…" : "Preparing audio…") }}</span>
          <button
            v-if="attachmentError(file)"
            type="button"
            :class="c.smallButton"
            @click.stop="downloadAttachment(file)"
          >
            Download
          </button>
        </div>
        <div :class="c.fileMeta">
          {{ file.name ?? "Audio" }} · {{ file.mediaType ?? "audio" }}
          <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
          <span v-if="file.encrypted" :class="c.encryptedInline"> · <Lock aria-hidden="true" /> Encrypted</span>
        </div>
      </div>
    </div>

    <!-- Inline PDF attachments (non-compact) / chip (compact) -->
    <!-- Downloadable attachments (non-compact) / chip (compact) -->
    <template v-if="!compact">
      <!-- Inline PDF attachments -->
      <div v-if="pdfAttachments.length > 0 && mediaShown" :class="c.column">
        <div
          v-for="file in pdfAttachments"
          :key="attachmentKey(file)"
          :class="c.card"
        >
          <iframe
            v-if="resolvedAttachmentUrl(file)"
            :src="resolvedAttachmentUrl(file) || ''"
            :title="file.name ?? 'PDF document'"
            :class="c.pdf"
          />
          <div
            v-else
            :class="cx(c.placeholder, c.dashed, c.h40)"
          >
            <Lock />
            <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting PDF…" : "Preparing PDF…") }}</span>
            <button
              v-if="attachmentError(file)"
              type="button"
              :class="c.smallButton"
              @click="downloadAttachment(file)"
            >
              Download
            </button>
          </div>
          <div :class="c.fileMeta">
            {{ file.name ?? "PDF" }} · {{ file.mediaType ?? "application/pdf" }}
            <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
            <span v-if="file.encrypted" :class="c.encryptedInline"> · <Lock aria-hidden="true" /> Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Downloadable attachments -->
      <div v-if="downloadableAttachments.length > 0" :class="c.columnTight">
        <template v-for="file in downloadableAttachments" :key="attachmentKey(file)">
          <button
            v-if="file.encrypted"
            type="button"
            :class="c.fileCard"
            @click="downloadAttachment(file)"
          >
            <FileDown />
            <div :class="c.fileInfo">
              <div :class="c.fileName">{{ file.name ?? "File" }}</div>
              <div :class="c.fileMetaRow">
                <span>{{ file.mediaType ?? "file" }}</span>
                <span v-if="file.size">· {{ formatFileSize(file.size) }}</span>
                <span :class="c.encryptedPill">
                  <Lock />
                  Encrypted
                </span>
              </div>
              <div v-if="attachmentError(file)" :class="c.fileError">
                {{ attachmentError(file) }}
              </div>
            </div>
          </button>
          <!-- href resolves via the attachment scheme allowlist; a disallowed
               scheme drops the attribute so the card renders inert. -->
          <a
            v-else
            :href="resolvedAttachmentUrl(file) ?? undefined"
            target="_blank"
            rel="noopener noreferrer"
            :class="c.fileCard"
          >
            <FileDown />
            <div :class="c.fileInfo">
              <div :class="c.fileName">{{ file.name ?? "File" }}</div>
              <div :class="c.fileMeta">
                {{ file.mediaType ?? "file" }}
                <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
              </div>
            </div>
          </a>
        </template>
      </div>
    </template>
    <template v-else>
      <!-- Compact: PDF + downloadables as uniform chip row -->
      <div
        v-if="(pdfAttachments.length + downloadableAttachments.length) > 0"
        :class="c.columnTight"
      >
        <template
          v-for="file in [...pdfAttachments, ...downloadableAttachments]"
          :key="attachmentKey(file)"
        >
          <a
            v-if="!file.encrypted || resolvedAttachmentUrl(file)"
            :href="resolvedAttachmentUrl(file) ?? undefined"
            target="_blank"
            rel="noopener noreferrer"
            :class="c.fileChip"
            @click.stop
          >
            <FileDown aria-hidden="true" />
            <span>{{ file.name ?? "Attachment" }}</span>
            <span v-if="file.size" :class="c.muted">{{ formatFileSize(file.size) }}</span>
          </a>
          <span
            v-else
            :class="c.fileChip"
            data-muted
            title="Decrypting…"
          >
            <Lock aria-hidden="true" />
            <span>{{ file.name ?? "Encrypted attachment" }}</span>
          </span>
        </template>
      </div>
    </template>
  </div>
</template>
