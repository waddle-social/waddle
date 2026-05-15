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
         Two intents, one shell family:
           event → notification-led (read-first). Title + structured meta
                   + optional secondary action chips. GitHub workflow runs,
                   check results, deploy status.
           tool  → action-led (do-first). Compact label + context line +
                   primary action button(s) right-aligned. Link Board,
                   polls, surveys.
         Both share `.chat-extension-card` for family resemblance — same
         soft surface, same tone-coloured left rail; layout differs by
         intent. -->
    <div v-if="extensionCards.length > 0" class="flex flex-col gap-2">
      <div
        v-for="card in extensionCards"
        :key="`${card.annotation.extensionId}:${card.annotation.annotationId}`"
        class="chat-extension-card"
        :class="[
          card.presentation.intent === 'tool'
            ? 'chat-extension-card--action'
            : 'chat-extension-card--notification',
          card.presentation.tone === 'success' ? 'chat-extension-card--tone-success' : '',
          card.presentation.tone === 'danger'  ? 'chat-extension-card--tone-danger'  : '',
          card.presentation.tone === 'warning' ? 'chat-extension-card--tone-warning' : '',
          compact ? 'chat-extension-card--compact' : '',
        ]"
      >
        <!-- === EVENT intent (notification) === -->
        <template v-if="card.presentation.intent === 'event'">
          <div class="chat-extension-card__header">
            <span class="chat-extension-card__icon" aria-hidden="true">
              <MessageSquare v-if="card.annotation.surfaceKind === 'chat-bot'" class="w-full h-full" />
              <Github v-else-if="card.presentation.kind === 'github-event'" class="w-full h-full" />
              <LayoutDashboard v-else class="w-full h-full" />
            </span>
            <span class="chat-extension-card__source">{{ card.presentation.label || extensionSurfaceLabel(card.annotation.surfaceKind) }}</span>
            <span v-if="card.presentation.primaryValue" class="chat-extension-card__tone-pill">
              {{ card.presentation.primaryValue }}
            </span>
          </div>
          <div class="chat-extension-card__title">
            <a
              v-if="card.presentation.primaryUrl && !compact"
              :href="card.presentation.primaryUrl"
              target="_blank"
              rel="noreferrer"
              class="chat-extension-card__title-link"
              @click.stop
            >
              <span class="min-w-0">{{ card.presentation.title }}</span>
              <ExternalLink aria-hidden="true" />
            </a>
            <template v-else>{{ card.presentation.title }}</template>
          </div>
          <div
            v-if="card.presentation.details.length > 0 || card.presentation.secondaryValue"
            class="chat-extension-card__meta"
          >
            <span
              v-if="card.presentation.secondaryValue"
              class="chat-extension-card__meta-item"
            >
              <span class="chat-extension-card__meta-value">{{ card.presentation.secondaryValue }}</span>
            </span>
            <span
              v-for="detail in card.presentation.details"
              :key="`${card.annotation.annotationId}:${detail.label}`"
              class="chat-extension-card__meta-item"
            >
              <span class="chat-extension-card__meta-label">{{ detail.label }}</span>
              <span
                class="chat-extension-card__meta-value"
                :class="detail.label === 'Commit' ? 'chat-extension-card__meta-value--mono' : ''"
                :title="detail.value"
              >{{ detail.value }}</span>
            </span>
          </div>
          <!-- Event-intent actions: secondary chips, never the focal point. -->
          <div
            v-if="!compact && card.annotation.actions.length > 0"
            class="chat-extension-card__actions"
            style="margin-top: 0.4rem;"
          >
            <button
              v-for="action in card.annotation.actions"
              :key="`${card.annotation.annotationId}:${action.route}:${action.label}`"
              type="button"
              class="chat-extension-card__action chat-extension-card__action--secondary"
              :disabled="extensionActionState(card.annotation.annotationId, action)?.state === 'loading' || !action.launch"
              :title="extensionActionState(card.annotation.annotationId, action)?.detail ?? action.launch?.commandNode ?? action.label"
              @click.stop="invokeExtension(card.annotation.annotationId, action)"
            >
              <LoaderCircle v-if="extensionActionState(card.annotation.annotationId, action)?.state === 'loading'" class="animate-spin" aria-hidden="true" />
              <CheckCircle2 v-else-if="extensionActionState(card.annotation.annotationId, action)?.state === 'success'" aria-hidden="true" />
              <AlertCircle v-else-if="extensionActionState(card.annotation.annotationId, action)?.state === 'warning'" aria-hidden="true" />
              <AlertCircle v-else-if="extensionActionState(card.annotation.annotationId, action)?.state === 'error'" aria-hidden="true" />
              {{ action.label }}
            </button>
          </div>
        </template>

        <!-- === TOOL intent (action) === -->
        <template v-else>
          <span class="chat-extension-card__icon" aria-hidden="true">
            <MessageSquare v-if="card.annotation.surfaceKind === 'chat-bot'" class="w-full h-full" />
            <Github v-else-if="card.presentation.kind === 'github-event'" class="w-full h-full" />
            <LayoutDashboard v-else class="w-full h-full" />
          </span>
          <div class="chat-extension-card__body">
            <div class="chat-extension-card__header">
              <span class="chat-extension-card__source">{{ card.presentation.label || extensionSurfaceLabel(card.annotation.surfaceKind) }}</span>
            </div>
            <div class="chat-extension-card__title">{{ card.presentation.title }}</div>
            <a
              v-if="card.presentation.primaryUrl && !compact"
              :href="card.presentation.primaryUrl"
              target="_blank"
              rel="noreferrer"
              class="chat-extension-card__url"
              @click.stop
            >{{ card.presentation.primaryUrl }}</a>
            <ul
              v-if="card.presentation.options.length > 0"
              class="chat-extension-card__meta"
              style="flex-direction: column; align-items: stretch;"
            >
              <li
                v-for="option in card.presentation.options"
                :key="`${card.annotation.annotationId}:option:${option.id}`"
                class="chat-extension-card__meta-item"
                style="justify-content: space-between;"
              >
                <span class="chat-extension-card__meta-value">{{ option.label }}</span>
                <span v-if="option.value !== undefined" class="chat-extension-card__meta-label">{{ option.value }}</span>
              </li>
            </ul>
          </div>
          <div v-if="!compact && card.annotation.actions.length > 0" class="chat-extension-card__actions">
            <button
              v-for="(action, index) in card.annotation.actions"
              :key="`${card.annotation.annotationId}:${action.route}:${action.label}`"
              type="button"
              class="chat-extension-card__action"
              :class="index === 0 ? '' : 'chat-extension-card__action--secondary'"
              :disabled="extensionActionState(card.annotation.annotationId, action)?.state === 'loading' || !action.launch"
              :title="extensionActionState(card.annotation.annotationId, action)?.detail ?? action.launch?.commandNode ?? action.label"
              @click.stop="invokeExtension(card.annotation.annotationId, action)"
            >
              <LoaderCircle v-if="extensionActionState(card.annotation.annotationId, action)?.state === 'loading'" class="animate-spin" aria-hidden="true" />
              <CheckCircle2 v-else-if="extensionActionState(card.annotation.annotationId, action)?.state === 'success'" aria-hidden="true" />
              <AlertCircle v-else-if="extensionActionState(card.annotation.annotationId, action)?.state === 'warning'" aria-hidden="true" />
              <AlertCircle v-else-if="extensionActionState(card.annotation.annotationId, action)?.state === 'error'" aria-hidden="true" />
              {{ action.label }}
            </button>
          </div>
        </template>

        <!-- Action error/warning detail strip (both intents) -->
        <template v-if="!compact">
          <span
            v-for="action in card.annotation.actions"
            :key="`${card.annotation.annotationId}:${action.route}:state`"
            v-show="extensionActionState(card.annotation.annotationId, action)?.detail && extensionActionState(card.annotation.annotationId, action)?.state !== 'success'"
            class="type-caption"
            :class="extensionActionState(card.annotation.annotationId, action)?.state === 'error' ? 'text-destructive' : 'text-warning'"
            style="flex-basis: 100%; margin-top: 0.25rem;"
          >
            {{ extensionActionState(card.annotation.annotationId, action)?.detail }}
          </span>
        </template>
      </div>
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
