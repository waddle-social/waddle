<script setup lang="ts">
// Presentational body + attachments + extension-annotation cards for
// a TimelineMessage. Used by `MessageCard` (timeline) and `PinnedPanel`
// (right rail). `compact` adapts layout for the narrow rail width and
// suppresses interactive affordances that belong on the timeline only
// (extension action buttons).
//
import { computed, ref, nextTick, watch } from "vue";
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
} from "lucide-vue-next";
import {
  extensionPresentation,
  extensionSurfaceLabel,
  renderStyledBody,
  type TimelineMessage,
  type ExtensionAnnotation,
  type ExtensionAnnotationAction,
} from "@/lib/chat-ui";
import { formatFileSize, useMessageAttachments } from "@/channels/message-attachments";
import { applyShikiToCodeBlocks } from "@/lib/shiki";
import { useExtensionAnnotationActions } from "@/channels/extension-annotation-actions";
import type { ExtensionCommandResult } from "@/lib/xmpp/extension-commands";
import ImageLightbox from "@/components/ui/ImageLightbox.vue";

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
} = useMessageAttachments(messageRef);

const extensionAnnotations = computed(() => props.message.extensionAnnotations ?? []);
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

const {
  actionState: extensionActionState,
  actionStatusLabel,
  invokeExtension,
} = useExtensionAnnotationActions({
  annotations: extensionAnnotations,
  invokeExtensionAction: invokeExtensionActionRef,
});

const isSticker = computed(() => !!props.message.isSticker && imageAttachments.value.length > 0);

const styledHtml = computed(() =>
  renderStyledBody(displayBody.value, props.message.markup, props.message.references),
);
const shouldRenderTextBody = computed(() =>
  !!displayBody.value && !props.message.extensionBodyFallback,
);
const styledBodyRef = ref<HTMLDivElement | null>(null);

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

</script>

<template>
  <div class="chat-message-media-stack">
    <!-- User text body (shown alongside attachments) -->
    <div
      v-if="shouldRenderTextBody"
      ref="styledBodyRef"
      :class="[
        'type-message-body styled-body',
        compact ? 'type-field-sm line-clamp-3' : '',
      ]"
      v-html="styledHtml"
    />

    <!-- Sticker: single image rendered at thumb size, with decryption support.
         v-else-if chains with displayBody and isInlineImageBodyUrl so only one body/media branch
         renders per message (XEP-0449 stickers carry a <body> alt-text that must
         not also render as a text bubble). -->
    <div v-else-if="isSticker">
      <img
        v-if="resolvedAttachmentUrl(imageAttachments[0])"
        :src="resolvedAttachmentUrl(imageAttachments[0])!"
        :alt="imageAttachments[0].desc ?? message.body ?? 'Sticker'"
        :class="[
          'rounded-lg object-contain',
          compact ? 'max-w-20 max-h-20' : 'max-w-28 max-h-28',
        ]"
        loading="lazy"
        @click.stop
      />
      <div
        v-else
        :class="[
          'type-caption flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground',
          compact ? 'h-20 w-20' : 'h-28 w-28',
        ]"
        @click.stop
      >
        <Lock class="h-4 w-4 text-primary/70" />
        <span>{{ attachmentError(imageAttachments[0]) ?? (isDecryptingAttachment(imageAttachments[0]) ? "Decrypting sticker…" : "Preparing sticker…") }}</span>
      </div>
    </div>

    <!-- Inline image body URL -->
    <div v-else-if="isInlineImageBodyUrl">
      <img
        :src="message.body.trim()"
        alt="Image"
        :class="[
          'rounded-lg border border-border object-contain cursor-pointer transition-opacity hover:opacity-90',
          compact ? 'max-h-24' : 'chat-attachment-image',
        ]"
        loading="lazy"
        @click.stop="openInlineImageLightbox"
      />
    </div>

    <!-- Waddle extension annotations.
         Event-intent (rare on human-authored messages — bot-authored
         events are intercepted upstream and rendered as full-width
         system bands by MessageCard) renders as a compact inline
         notification card.
         Tool-intent annotations from ANY number of extensions collapse
         into a single horizontal action-chip strip beneath this block.
         No fat boxes fanning out below the message. -->
    <section
      v-for="card in eventCards"
      :key="`${card.annotation.extensionId}:${card.annotation.annotationId}`"
      class="chat-system-band chat-system-band--inline"
      :class="[
        card.presentation.tone === 'success' ? 'chat-system-band--tone-success' : '',
        card.presentation.tone === 'danger'  ? 'chat-system-band--tone-danger'  : '',
        card.presentation.tone === 'warning' ? 'chat-system-band--tone-warning' : '',
        card.presentation.kind ? `chat-system-band--kind-${card.presentation.kind}` : '',
      ]"
    >
      <div class="chat-system-band__header">
        <span class="chat-system-band__source">
          <MessageSquare v-if="card.annotation.surfaceKind === 'chat-bot'" aria-hidden="true" />
          <Github v-else-if="card.presentation.kind === 'github-event'" aria-hidden="true" />
          <LayoutDashboard v-else aria-hidden="true" />
          {{ card.presentation.label || extensionSurfaceLabel(card.annotation.surfaceKind) }}
        </span>
        <span v-if="card.presentation.primaryValue" class="chat-system-band__tone-pill">{{ card.presentation.primaryValue }}</span>
      </div>
      <div class="chat-system-band__title">
        <a
          v-if="card.presentation.primaryUrl && !compact"
          :href="card.presentation.primaryUrl"
          target="_blank"
          rel="noreferrer"
          class="chat-system-band__title-link"
          @click.stop
        >
          <span>{{ card.presentation.title }}</span>
          <ExternalLink aria-hidden="true" />
        </a>
        <template v-else>{{ card.presentation.title }}</template>
      </div>
      <div
        v-if="card.presentation.details.length > 0 || card.presentation.secondaryValue"
        class="chat-system-band__meta"
      >
        <span v-if="card.presentation.secondaryValue" class="chat-system-band__meta-item">
          <span class="chat-system-band__meta-value">{{ card.presentation.secondaryValue }}</span>
        </span>
        <span
          v-for="detail in card.presentation.details"
          :key="`${card.annotation.annotationId}:${detail.label}`"
          class="chat-system-band__meta-item"
        >
          <span class="chat-system-band__meta-label">{{ detail.label }}</span>
          <span
            class="chat-system-band__meta-value"
            :class="(detail.label === 'Commit' || detail.label === 'Branch') ? 'chat-system-band__meta-value--mono' : ''"
            :title="detail.value"
          >{{ detail.value }}</span>
        </span>
      </div>
    </section>

    <!-- Tool-intent extension actions — a single horizontal chip strip
         under the message body. Multiple extensions each contributing
         actions to the same message all collapse here; no fat boxes
         fan out below.  -->
    <div v-if="!compact && toolActionChips.length > 0" class="chat-extension-action-strip">
      <button
        v-for="chip in toolActionChips"
        :key="`${chip.annotation.annotationId}:${chip.action.route}`"
        type="button"
        class="chat-extension-action-chip"
        :class="extensionActionState(chip.annotation.annotationId, chip.action)?.state
          ? `chat-extension-action-chip--state-${extensionActionState(chip.annotation.annotationId, chip.action)?.state}`
          : ''"
        :disabled="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'loading' || !chip.action.launch"
        :title="extensionActionState(chip.annotation.annotationId, chip.action)?.detail
          ?? `${chip.presentation.label}: ${chip.action.label}`"
        @click.stop="invokeExtension(chip.annotation.annotationId, chip.action)"
      >
        <LoaderCircle v-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'loading'" class="animate-spin" aria-hidden="true" />
        <CheckCircle2 v-else-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'success'" aria-hidden="true" />
        <AlertCircle v-else-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'warning'" aria-hidden="true" />
        <AlertCircle v-else-if="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'error'" aria-hidden="true" />
        <MessageSquare v-else-if="chip.annotation.surfaceKind === 'chat-bot'" aria-hidden="true" />
        <Github v-else-if="chip.presentation.kind === 'github-event'" aria-hidden="true" />
        <LayoutDashboard v-else aria-hidden="true" />
        <span class="chat-extension-action-chip__source">{{ chip.presentation.label }}</span>
        <span>{{ chip.action.label }}</span>
      </button>
      <template v-for="chip in toolActionChips" :key="`${chip.annotation.annotationId}:${chip.action.route}:detail`">
        <span
          v-if="extensionActionState(chip.annotation.annotationId, chip.action)?.detail && extensionActionState(chip.annotation.annotationId, chip.action)?.state !== 'success'"
          class="chat-extension-action-detail"
          :class="extensionActionState(chip.annotation.annotationId, chip.action)?.state === 'error' ? 'chat-extension-action-detail--error' : 'chat-extension-action-detail--warning'"
        >
          {{ extensionActionState(chip.annotation.annotationId, chip.action)?.detail }}
        </span>
      </template>
    </div>

    <!-- Image attachments gallery -->
    <div v-if="imageAttachments.length > 0 && !isSticker" class="chat-attachment-strip">
      <div
        v-for="img in imageAttachments"
        :key="attachmentRenderKey(img)"
        class="rounded-lg border border-border overflow-hidden bg-muted/40"
      >
        <button
          v-if="resolvedAttachmentUrl(img)"
          type="button"
          class="block hover:opacity-90 transition-opacity focus-visible:outline-2 focus-visible:outline-primary"
          :title="img.name ?? 'Image'"
          @click.stop="openLightbox(img)"
        >
          <img
            :src="resolvedAttachmentUrl(img) || ''"
            :alt="img.name ?? 'Shared image'"
            :class="[
              'object-cover',
              compact ? 'h-20 w-20' : 'chat-attachment-image',
            ]"
            loading="lazy"
          />
        </button>
        <div
          v-else
          :class="[
            'type-caption flex flex-col items-center justify-center gap-2 px-4 text-center text-muted-foreground',
            compact ? 'h-20 w-20' : 'h-36 w-48',
          ]"
          @click.stop
        >
          <Lock class="h-4 w-4 text-primary/70" />
          <span>{{ attachmentError(img) ?? (isDecryptingAttachment(img) ? "Decrypting image…" : "Preparing image…") }}</span>
          <button
            v-if="attachmentError(img) && !compact"
            type="button"
            class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
            @click.stop="downloadAttachment(img)"
          >
            Download
          </button>
        </div>
        <div
          v-if="img.encrypted"
          class="type-meta type-emphasis flex items-center gap-1 border-t border-border/70 px-2 py-1 text-muted-foreground"
          @click.stop
        >
          <Lock class="h-3 w-3 text-primary/70" />
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
    <div v-if="videoAttachments.length > 0" class="flex flex-col gap-2">
      <div
        v-for="file in videoAttachments"
        :key="attachmentKey(file)"
        class="chat-attachment-card flex flex-col gap-2 rounded-lg border border-border bg-muted/30 p-2"
      >
        <video
          v-if="resolvedAttachmentUrl(file)"
          :src="resolvedAttachmentUrl(file) || ''"
          class="max-h-72 w-full rounded-lg border border-border bg-black"
          controls
          playsinline
          :preload="compact ? 'none' : 'metadata'"
          @click.stop
        />
        <div
          v-else
          class="type-caption flex h-40 w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground"
        >
          <Lock class="h-4 w-4 text-primary/70" />
          <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting video…" : "Preparing video…") }}</span>
          <button
            v-if="attachmentError(file)"
            type="button"
            class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
            @click.stop="downloadAttachment(file)"
          >
            Download
          </button>
        </div>
        <div class="type-caption text-muted-foreground">
          {{ file.name ?? "Video" }} · {{ file.mediaType ?? "video" }}
          <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
          <span v-if="file.encrypted"> · Encrypted</span>
        </div>
      </div>
    </div>

    <!-- Inline audio attachments -->
    <div v-if="audioAttachments.length > 0" class="flex flex-col gap-2">
      <div
        v-for="file in audioAttachments"
        :key="attachmentKey(file)"
        class="chat-attachment-card flex flex-col gap-2 rounded-lg border border-border bg-muted/30 p-3"
      >
        <audio
          v-if="resolvedAttachmentUrl(file)"
          :src="resolvedAttachmentUrl(file) || ''"
          class="w-full"
          controls
          :preload="compact ? 'none' : 'metadata'"
          @click.stop
        />
        <div
          v-else
          class="type-caption flex min-h-20 w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground"
        >
          <Lock class="h-4 w-4 text-primary/70" />
          <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting audio…" : "Preparing audio…") }}</span>
          <button
            v-if="attachmentError(file)"
            type="button"
            class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
            @click.stop="downloadAttachment(file)"
          >
            Download
          </button>
        </div>
        <div class="type-caption text-muted-foreground">
          {{ file.name ?? "Audio" }} · {{ file.mediaType ?? "audio" }}
          <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
          <span v-if="file.encrypted"> · Encrypted</span>
        </div>
      </div>
    </div>

    <!-- Inline PDF attachments (non-compact) / chip (compact) -->
    <!-- Downloadable attachments (non-compact) / chip (compact) -->
    <template v-if="!compact">
      <!-- Inline PDF attachments -->
      <div v-if="pdfAttachments.length > 0" class="flex flex-col gap-2">
        <div
          v-for="file in pdfAttachments"
          :key="attachmentKey(file)"
          class="chat-attachment-card flex flex-col gap-2 rounded-lg border border-border bg-muted/30 p-2"
        >
          <iframe
            v-if="resolvedAttachmentUrl(file)"
            :src="resolvedAttachmentUrl(file) || ''"
            :title="file.name ?? 'PDF document'"
            class="h-72 w-full rounded-lg border border-border bg-background"
          />
          <div
            v-else
            class="type-caption flex h-40 w-full flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border px-4 text-center text-muted-foreground"
          >
            <Lock class="h-4 w-4 text-primary/70" />
            <span>{{ attachmentError(file) ?? (isDecryptingAttachment(file) ? "Decrypting PDF…" : "Preparing PDF…") }}</span>
            <button
              v-if="attachmentError(file)"
              type="button"
              class="type-caption rounded-lg border border-border bg-background px-2.5 py-1 text-foreground hover:bg-muted transition-colors"
              @click="downloadAttachment(file)"
            >
              Download
            </button>
          </div>
          <div class="type-caption text-muted-foreground">
            {{ file.name ?? "PDF" }} · {{ file.mediaType ?? "application/pdf" }}
            <span v-if="file.size"> · {{ formatFileSize(file.size) }}</span>
            <span v-if="file.encrypted"> · Encrypted</span>
          </div>
        </div>
      </div>

      <!-- Downloadable attachments -->
      <div v-if="downloadableAttachments.length > 0" class="flex flex-col gap-1.5">
        <template v-for="file in downloadableAttachments" :key="attachmentKey(file)">
          <button
            v-if="file.encrypted"
            type="button"
            class="chat-file-card inline-flex items-center gap-3 bg-muted rounded-lg p-3 hover:bg-muted/80 transition-all duration-200 text-left"
            @click="downloadAttachment(file)"
          >
            <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
            <div class="flex-1 min-w-0">
              <div class="type-control truncate">{{ file.name ?? "File" }}</div>
              <div class="type-caption flex flex-wrap items-center gap-1.5 text-muted-foreground">
                <span>{{ file.mediaType ?? "file" }}</span>
                <span v-if="file.size">· {{ formatFileSize(file.size) }}</span>
                <span class="inline-flex items-center gap-1 rounded-full bg-primary/8 px-1.5 py-0.5 text-primary/80">
                  <Lock class="h-3 w-3" />
                  Encrypted
                </span>
              </div>
              <div v-if="attachmentError(file)" class="type-caption text-destructive">
                {{ attachmentError(file) }}
              </div>
            </div>
          </button>
          <a
            v-else
            :href="file.url"
            target="_blank"
            rel="noopener noreferrer"
            class="chat-file-card inline-flex items-center gap-3 bg-muted rounded-lg p-3 hover:bg-muted/80 transition-all duration-200"
          >
            <FileDown class="w-4 h-4 text-muted-foreground flex-shrink-0" />
            <div class="flex-1 min-w-0">
              <div class="type-control truncate">{{ file.name ?? "File" }}</div>
              <div class="type-caption text-muted-foreground">
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
        class="flex flex-col gap-1.5"
      >
        <template
          v-for="file in [...pdfAttachments, ...downloadableAttachments]"
          :key="attachmentKey(file)"
        >
          <a
            v-if="!file.encrypted || resolvedAttachmentUrl(file)"
            :href="resolvedAttachmentUrl(file) || file.url || '#'"
            target="_blank"
            rel="noopener noreferrer"
            class="type-caption inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2 py-1 text-foreground hover:bg-muted"
            @click.stop
          >
            <FileDown class="h-3 w-3" aria-hidden="true" />
            <span class="truncate">{{ file.name ?? "Attachment" }}</span>
            <span v-if="file.size" class="text-muted-foreground">{{ formatFileSize(file.size) }}</span>
          </a>
          <span
            v-else
            class="type-caption inline-flex items-center gap-1.5 rounded-md border border-border bg-muted/50 px-2 py-1 text-muted-foreground"
            title="Decrypting…"
          >
            <Lock class="h-3 w-3" aria-hidden="true" />
            <span class="truncate">{{ file.name ?? "Encrypted attachment" }}</span>
          </span>
        </template>
      </div>
    </template>
  </div>
</template>
